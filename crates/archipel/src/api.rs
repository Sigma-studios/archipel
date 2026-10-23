//! The HTTP API (see docs/api.md) and the web UI next to it.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use archipel_core::api::*;
use archipel_core::game::GameDef;
use archipel_core::template;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::body::Body;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use rand::RngCore;
use serde::Deserialize;
use sha2::Digest;
use tracing::info;

use crate::config::Config;
use crate::games::Games;
use crate::store::{NewServer, ServerRow, Store, UserRow};
use crate::supervisor::Supervisor;
use crate::{files, fsops};

const SESSION_DAYS: u64 = 30;

pub struct App {
    pub cfg: Config,
    pub store: Arc<Store>,
    pub games: Arc<Games>,
    pub supervisor: Supervisor,
}

type S = State<Arc<App>>;
type ApiResult<T> = Result<T, ApiError>;

pub fn router(app: Arc<App>) -> Router {
    let desktop = app.cfg.mode() == Mode::Desktop;
    let router = Router::new()
        .route("/api/about", get(about))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/me/password", put(change_password))
        .route("/api/games", get(games))
        .route("/api/games/{game}/options/{key}", get(setting_options))
        .route("/api/servers", get(list_servers).post(create_server))
        .route(
            "/api/servers/{id}",
            get(get_server).put(update_server).delete(delete_server),
        )
        .route("/api/servers/{id}/start", post(start_server))
        .route("/api/servers/{id}/stop", post(stop_server))
        .route("/api/servers/{id}/restart", post(restart_server))
        .route("/api/servers/{id}/reinstall", post(reinstall_server))
        .route("/api/servers/{id}/console", get(console).post(command))
        .route("/api/servers/{id}/console/lines", get(console_lines))
        .route("/api/servers/{id}/files", get(list_files).put(save_file))
        .route("/api/overview", get(overview))
        .route("/api/internet", get(internet))
        .route("/api/internet/check", post(check_internet))
        .route("/api/users", get(list_users).post(create_user))
        .route(
            "/api/users/{username}",
            put(update_user).delete(delete_user),
        )
        .fallback(crate::ui::serve);
    let router = if desktop {
        router.layer(axum::middleware::from_fn(only_this_computer))
    } else {
        router
    };
    router.with_state(app)
}

/// The desktop has no login: only its own window may call (or, developing, a
/// browser on this computer), not a web page elsewhere aiming at a local port
/// (through DNS rebinding, or a form posted across sites).
async fn only_this_computer(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    fn local(url_host: &str) -> bool {
        let host = url_host
            .rsplit_once(':')
            .filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit()))
            .map_or(url_host, |(host, _)| host);
        matches!(host, "localhost" | "127.0.0.1" | "[::1]")
    }
    let headers = req.headers();
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .or(req.uri().authority().map(|a| a.as_str()));
    let origin_ok = match headers.get(header::ORIGIN).map(|o| o.to_str()) {
        None => true,
        Some(Ok(origin)) => origin.split_once("://").is_some_and(|(scheme, host)| {
            matches!(scheme, "archipel" | "http" | "tauri") && local(host)
        }),
        Some(Err(_)) => false,
    };
    if !host.is_some_and(local) || !origin_ok {
        return ApiError(
            StatusCode::FORBIDDEN,
            "only this computer's archipel window".into(),
        )
        .into_response();
    }
    next.run(req).await
}

async fn about(State(app): S) -> Json<About> {
    Json(About {
        mode: app.cfg.mode(),
        version: match option_env!("ARCHIPEL_COMMIT") {
            Some(commit) => format!("{} ({commit})", env!("CARGO_PKG_VERSION")),
            None => env!("CARGO_PKG_VERSION").into(),
        },
    })
}

// ---------------------------------------------------------------- errors

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(ErrorBody { error: self.1 })).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

fn bad(msg: impl ToString) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.to_string())
}
fn not_found(msg: impl ToString) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, msg.to_string())
}
fn forbidden(msg: impl ToString) -> ApiError {
    ApiError(StatusCode::FORBIDDEN, msg.to_string())
}
/// A refusal of what was asked, with its reason.
fn refused(e: anyhow::Error) -> ApiError {
    ApiError(StatusCode::CONFLICT, format!("{e:#}"))
}

// ---------------------------------------------------------------- auth

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let mut salt = [0u8; 16];
    rand::rng().fill_bytes(&mut salt);
    let salt = SaltString::encode_b64(&salt).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(argon2::Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .to_string())
}

fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| {
        argon2::Argon2::default()
            .verify_password(password.as_bytes(), &h)
            .is_ok()
    })
}

fn hash_token(token: &str) -> String {
    hex::encode(sha2::Sha256::digest(token.as_bytes()))
}

pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Who calls.
pub struct Caller {
    pub user: UserRow,
    token_hash: String,
}

impl Caller {
    fn is_admin(&self) -> bool {
        self.user.role == Role::Admin
    }

    fn admin(&self) -> ApiResult<()> {
        if self.is_admin() {
            Ok(())
        } else {
            Err(forbidden("only admins can do that"))
        }
    }
}

impl FromRequestParts<Arc<App>> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> ApiResult<Self> {
        if app.cfg.mode() == Mode::Desktop {
            // No login: the person at the computer owns everything.
            let user = app
                .store
                .user_by_name(OWNER)?
                .ok_or_else(|| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "no owner".into()))?;
            return Ok(Caller {
                user,
                token_hash: String::new(),
            });
        }
        let unauthorized = || ApiError(StatusCode::UNAUTHORIZED, "please log in".into());
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(unauthorized)?;
        let token_hash = hash_token(token.trim());
        let user = app
            .store
            .session_user(&token_hash)?
            .ok_or_else(unauthorized)?;
        Ok(Caller { user, token_hash })
    }
}

/// Creates the first admin when there is no user yet.
pub fn ensure_admin(app: &App) -> anyhow::Result<()> {
    if app.store.count_users()? > 0 {
        return Ok(());
    }
    let (password, told) = match &app.cfg.admin_password {
        Some(p) => (
            p.clone(),
            "the password in ARCHIPEL_ADMIN_PASSWORD".to_string(),
        ),
        None => {
            let p = random_token()[..20].to_string();
            let path = app.cfg.data_dir.join("initial-admin-password");
            write_private(&path, &p)?;
            (
                p.clone(),
                format!("password {p} (also in {})", path.display()),
            )
        }
    };
    let quota = Quota {
        memory_mb: 1024 * 1024,
        cpu_percent: 100 * 1024,
        max_servers: 1000,
        disk_mb: 1024 * 1024 * 1024,
    };
    app.store
        .create_user("admin", &hash_password(&password)?, Role::Admin, &quota)?;
    info!("created user `admin` with {told}; change it from the Account page");
    Ok(())
}

/// The desktop's one user, whom every request is from.
const OWNER: &str = "you";

/// Creates the desktop's user, admin of no one but themselves, without limits.
pub fn ensure_owner(app: &App) -> anyhow::Result<()> {
    if app.store.user_by_name(OWNER)?.is_some() {
        return Ok(());
    }
    let quota = Quota {
        memory_mb: 1024 * 1024,
        cpu_percent: 100 * 1024,
        max_servers: 1000,
        disk_mb: 1024 * 1024 * 1024,
    };
    // No password logs in as them: there is no login.
    let password = hash_password(&random_token())?;
    app.store
        .create_user(OWNER, &password, Role::Admin, &quota)?;
    Ok(())
}

pub fn write_private(path: &std::path::Path, content: &str) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(content.as_bytes())?;
    Ok(())
}

async fn login(State(app): S, Json(req): Json<LoginRequest>) -> ApiResult<Json<LoginResponse>> {
    if app.cfg.mode() == Mode::Desktop {
        return Err(not_found("the desktop app has no login"));
    }
    let user = app.store.user_by_name(req.username.trim())?;
    let Some(user) = user.filter(|u| verify_password(&req.password, &u.password_hash)) else {
        // Slow guessing down.
        tokio::time::sleep(Duration::from_millis(800)).await;
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "wrong username or password".into(),
        ));
    };
    let token = random_token();
    app.store.create_session(
        &hash_token(&token),
        user.id,
        archipel_core::now() + SESSION_DAYS * 86400,
    )?;
    Ok(Json(LoginResponse {
        token,
        user: user_view(&app, &user),
    }))
}

async fn logout(State(app): S, caller: Caller) -> ApiResult<StatusCode> {
    if !caller.token_hash.is_empty() {
        app.store.delete_session(&caller.token_hash)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn me(State(app): S, caller: Caller) -> Json<UserView> {
    Json(user_view(&app, &caller.user))
}

async fn change_password(
    State(app): S,
    caller: Caller,
    Json(req): Json<ChangePassword>,
) -> ApiResult<StatusCode> {
    if !verify_password(&req.current, &caller.user.password_hash) {
        return Err(bad("the current password is wrong"));
    }
    check_password(&req.new)?;
    let mut user = caller.user.clone();
    user.password_hash = hash_password(&req.new)?;
    app.store.update_user(&user)?;
    // Logged out everywhere else.
    app.store
        .delete_sessions_of(user.id, Some(&caller.token_hash))?;
    Ok(StatusCode::NO_CONTENT)
}

fn check_password(password: &str) -> ApiResult<()> {
    if password.chars().count() < 8 {
        return Err(bad("passwords have at least 8 characters"));
    }
    Ok(())
}

/// A quota as far as the machine can honour it: no share is larger than what
/// archipel has (its container's memory and CPU, its volume, its port pool, one
/// port per server at least). Admins' "everything" reads as the machine itself.
fn effective(app: &App, quota: &Quota) -> Quota {
    let (_, cpu) = app.supervisor.cgroups().totals();
    let memory = app.supervisor.memory_available_mb();
    let disk = disk_space(&app.cfg.data_dir).map(|(total, _)| total / (1024 * 1024));
    let ports = app
        .cfg
        .port_range()
        .map(|(first, last)| u32::from(last - first) + 1)
        .ok();
    let min = |want: u64, have: Option<u64>| have.map_or(want, |h| want.min(h));
    Quota {
        memory_mb: min(quota.memory_mb, memory),
        cpu_percent: min(u64::from(quota.cpu_percent), cpu.map(u64::from)) as u32,
        max_servers: min(u64::from(quota.max_servers), ports.map(u64::from)) as u32,
        disk_mb: min(quota.disk_mb, disk),
    }
}

fn user_view(app: &App, user: &UserRow) -> UserView {
    let rows = app.store.list_servers().unwrap_or_default();
    let mine: Vec<&ServerRow> = rows.iter().filter(|r| r.owner_id == user.id).collect();
    let (by_owner, _) = app.supervisor.memory_in_use();
    let running = mine
        .iter()
        .filter(|r| app.supervisor.instance(r.id).status().state.is_active())
        .count() as u32;
    let disk: u64 = mine
        .iter()
        .filter_map(|r| app.supervisor.disk_bytes(r.id))
        .sum();
    UserView {
        username: user.username.clone(),
        role: user.role,
        quota: effective(app, &user.quota),
        usage: Usage {
            memory_mb: by_owner.get(&user.id).copied().unwrap_or(0),
            servers: mine.len() as u32,
            running,
            disk_mb: disk / (1024 * 1024),
        },
        created_at: user.created_at,
    }
}

// ---------------------------------------------------------------- games and servers

async fn games(State(app): S, _: Caller) -> Json<Vec<GameDef>> {
    Json(app.games.list())
}

/// The choices of a setting whose options archipel lists (`options_from`), given
/// the other settings chosen so far (query parameters): the definition's own
/// options, then the listed ones.
async fn setting_options(
    State(app): S,
    _: Caller,
    Path((game, key)): Path<(String, String)>,
    Query(chosen): Query<std::collections::HashMap<String, String>>,
) -> ApiResult<Json<Vec<archipel_core::game::ChoiceOption>>> {
    use archipel_core::game::{ChoiceOption, SettingKind};
    let def = app
        .games
        .get(&game)
        .and_then(|g| g.setting(&key))
        .ok_or_else(|| not_found(format!("no setting {key} in {game}")))?;
    let SettingKind::Choice {
        options,
        options_from: Some(from),
        ..
    } = &def.kind
    else {
        return Err(not_found(format!("{key} has no listed options")));
    };
    let listed = match from.as_str() {
        "minecraft_versions" => {
            let paper = chosen.get("flavor").is_some_and(|f| f == "paper");
            app.supervisor
                .installer()
                .minecraft_versions(paper)
                .await
                .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}")))?
        }
        other => return Err(not_found(format!("no option source {other}"))),
    };
    let newest = listed.first().cloned();
    let mut out: Vec<ChoiceOption> = options
        .iter()
        .map(|o| ChoiceOption {
            // "Latest release (26.3)": what latest means right now.
            label: match (&newest, o.value.as_str()) {
                (Some(v), "latest") => format!("{} ({v})", o.label),
                _ => o.label.clone(),
            },
            value: o.value.clone(),
        })
        .collect();
    out.extend(listed.into_iter().map(|v| ChoiceOption {
        label: v.clone(),
        value: v,
    }));
    Ok(Json(out))
}

/// The server `id`, if the caller may see it.
fn load(app: &App, caller: &Caller, id: u64) -> ApiResult<ServerRow> {
    let row = app
        .store
        .get_server(id)?
        .ok_or_else(|| not_found("no such server"))?;
    if row.owner_id != caller.user.id && !caller.is_admin() {
        return Err(not_found("no such server"));
    }
    Ok(row)
}

fn server_view(app: &App, row: &ServerRow) -> ServerView {
    let inst = app.supervisor.instance(row.id);
    let status = inst.status();
    let game = app.games.get(&row.game);
    let owner = app
        .store
        .user_by_id(row.owner_id)
        .ok()
        .flatten()
        .map(|u| u.username)
        .unwrap_or_default();
    let connect_at = |host: String| {
        let g = game?;
        let template = g.connect.as_ref()?;
        let settings = g.settings_from(&row.settings).unwrap_or_default();
        let dir = app.cfg.server_dir(row.id);
        let mut ctx = crate::supervisor::context(g, row, &settings, &dir, None, None);
        ctx.insert("host".into(), Value::Text(host));
        template::render(template, &ctx).ok()
    };
    let (connect, reach) = match app.supervisor.reacher() {
        // Left for the page to fill in, unless archipel knows.
        None => (
            connect_at(
                app.cfg
                    .public_host
                    .clone()
                    .unwrap_or_else(|| "{host}".into()),
            ),
            None,
        ),
        Some(reacher) => match reacher.status(row.id) {
            None => (
                connect_at(
                    crate::reach::local_ip().map_or("localhost".into(), |ip| ip.to_string()),
                ),
                None,
            ),
            Some(status) => {
                let local = status.local_ip.and_then(|ip| connect_at(ip.to_string()));
                let public = status
                    .public_ip
                    .filter(|_| status.state == ReachState::Internet)
                    .and_then(|ip| {
                        connect_at(match ip {
                            std::net::IpAddr::V4(ip) => ip.to_string(),
                            std::net::IpAddr::V6(ip) => format!("[{ip}]"),
                        })
                    });
                let forward = match (status.state, game) {
                    (ReachState::LocalOnly, Some(g)) => g
                        .ports
                        .iter()
                        .filter_map(|p| {
                            let number = row.ports.get(&p.name)?;
                            let protocol = match p.protocol {
                                archipel_core::game::Protocol::Tcp => "TCP",
                                archipel_core::game::Protocol::Udp => "UDP",
                            };
                            Some(format!("{protocol} {number}"))
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                (
                    public.clone().or(local.clone()),
                    Some(Reach {
                        state: status.state,
                        local,
                        public,
                        forward,
                        detail: status.detail,
                        firewall: app
                            .cfg
                            .port_range()
                            .ok()
                            .and_then(|(first, last)| crate::reach::firewall_hint(first, last)),
                    }),
                )
            }
        },
    };
    ServerView {
        id: row.id,
        name: row.name.clone(),
        owner,
        game: row.game.clone(),
        game_name: game
            .map(|g| g.name.clone())
            .unwrap_or_else(|| row.game.clone()),
        memory_mb: row.memory_mb,
        settings: row.settings.clone(),
        ports: row.ports.clone(),
        state: status.state,
        message: status.message,
        started_at: status.started_at,
        usage: inst.usage(),
        disk_bytes: app.supervisor.disk_bytes(row.id),
        connect,
        reach,
        auto_start: row.auto_start,
        created_at: row.created_at,
    }
}

async fn list_servers(State(app): S, caller: Caller) -> ApiResult<Json<Vec<ServerView>>> {
    let rows = app.store.list_servers()?;
    Ok(Json(
        rows.iter()
            .filter(|r| caller.is_admin() || r.owner_id == caller.user.id)
            .map(|r| server_view(&app, r))
            .collect(),
    ))
}

fn check_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 40 || name.chars().any(char::is_control) {
        return Err(bad("a server's name has 1 to 40 characters"));
    }
    Ok(name.to_string())
}

async fn create_server(
    State(app): S,
    caller: Caller,
    Json(req): Json<CreateServer>,
) -> ApiResult<Json<ServerView>> {
    let owner = match &req.owner {
        Some(name) if name != &caller.user.username => {
            caller.admin()?;
            app.store
                .user_by_name(name)?
                .ok_or_else(|| not_found(format!("no user named {name}")))?
        }
        _ => caller.user.clone(),
    };
    let name = check_name(&req.name)?;
    let game = app
        .games
        .get(&req.game)
        .ok_or_else(|| not_found(format!("no game {}", req.game)))?;
    let settings = game.settings_from(&req.settings).map_err(bad)?;
    if req.memory_mb < game.memory.min_mb {
        return Err(bad(format!(
            "{} needs at least {} MB of memory",
            game.name, game.memory.min_mb
        )));
    }
    let quota = effective(&app, &owner.quota);
    if req.memory_mb > quota.memory_mb {
        return Err(bad(format!(
            "that is more memory than {}'s share ({} MB)",
            owner.username, quota.memory_mb
        )));
    }
    let count = app
        .store
        .list_servers()?
        .iter()
        .filter(|r| r.owner_id == owner.id)
        .count();
    if count as u32 >= quota.max_servers {
        return Err(refused(anyhow::anyhow!(
            "{} servers is as many as the quota allows: delete one first",
            quota.max_servers
        )));
    }
    let port_names: Vec<String> = game.ports.iter().map(|p| p.name.clone()).collect();
    let row = app
        .store
        .create_server(
            &NewServer {
                owner_id: owner.id,
                name: &name,
                game: &game.id,
                memory_mb: req.memory_mb,
                settings: &settings,
                port_names: &port_names,
            },
            app.cfg.port_range()?,
        )
        .map_err(refused)?;
    info!(server = row.id, game = %game.id, owner = %owner.username, "created {name}");
    // Created either way; if it cannot start now, it says why.
    if req.start
        && let Err(e) = app.supervisor.start(row.id)
    {
        app.supervisor.not_started(row.id, &e);
    }
    let row = app.store.get_server(row.id)?.unwrap_or(row);
    Ok(Json(server_view(&app, &row)))
}

async fn get_server(
    State(app): S,
    caller: Caller,
    Path(id): Path<u64>,
) -> ApiResult<Json<ServerView>> {
    let row = load(&app, &caller, id)?;
    Ok(Json(server_view(&app, &row)))
}

async fn update_server(
    State(app): S,
    caller: Caller,
    Path(id): Path<u64>,
    Json(req): Json<UpdateServer>,
) -> ApiResult<Json<ServerView>> {
    let mut row = load(&app, &caller, id)?;
    let game = app
        .games
        .get(&row.game)
        .ok_or_else(|| not_found("archipel no longer knows this server's game"))?;
    if let Some(name) = &req.name {
        row.name = check_name(name)?;
    }
    if let Some(mb) = req.memory_mb {
        let owner = app
            .store
            .user_by_id(row.owner_id)?
            .context("the server's owner is gone")?;
        let quota = effective(&app, &owner.quota);
        if mb < game.memory.min_mb || mb > quota.memory_mb {
            return Err(bad(format!(
                "memory must be between {} MB and the quota's {} MB",
                game.memory.min_mb, quota.memory_mb
            )));
        }
        row.memory_mb = mb;
    }
    if let Some(given) = &req.settings {
        let mut merged = row.settings.clone();
        merged.extend(given.clone());
        let settings = game.settings_from(&merged).map_err(bad)?;
        // A different version of the game: its files are installed again.
        let reinstall = game
            .settings
            .iter()
            .any(|s| s.reinstall && settings.get(&s.key) != row.settings.get(&s.key));
        row.settings = settings;
        if reinstall {
            app.supervisor
                .instance(id)
                .console
                .system("its game files will be updated at the next start".into());
        }
    }
    app.store.update_server(&row)?;
    Ok(Json(server_view(&app, &row)))
}

async fn delete_server(
    State(app): S,
    caller: Caller,
    Path(id): Path<u64>,
) -> ApiResult<StatusCode> {
    let row = load(&app, &caller, id)?;
    app.supervisor.delete(id).await?;
    app.store.delete_server(id)?;
    info!(server = id, "deleted {}", row.name);
    Ok(StatusCode::NO_CONTENT)
}

async fn start_server(
    State(app): S,
    caller: Caller,
    Path(id): Path<u64>,
) -> ApiResult<Json<ServerView>> {
    let row = load(&app, &caller, id)?;
    app.supervisor.start(id).map_err(refused)?;
    Ok(Json(server_view(&app, &row)))
}

async fn stop_server(
    State(app): S,
    caller: Caller,
    Path(id): Path<u64>,
) -> ApiResult<Json<ServerView>> {
    let row = load(&app, &caller, id)?;
    // Answers at once; the state tells when it is down.
    let supervisor = app.supervisor.clone();
    tokio::spawn(async move {
        if let Err(e) = supervisor.stop(id, true).await {
            tracing::warn!(server = id, "stopping: {e:#}");
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok(Json(server_view(&app, &row)))
}

async fn restart_server(
    State(app): S,
    caller: Caller,
    Path(id): Path<u64>,
) -> ApiResult<Json<ServerView>> {
    let row = load(&app, &caller, id)?;
    let supervisor = app.supervisor.clone();
    tokio::spawn(async move {
        if let Err(e) = supervisor.restart(id).await {
            supervisor
                .instance(id)
                .console
                .system(format!("not started again: {e:#}"));
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok(Json(server_view(&app, &row)))
}

async fn reinstall_server(
    State(app): S,
    caller: Caller,
    Path(id): Path<u64>,
) -> ApiResult<Json<ServerView>> {
    let row = load(&app, &caller, id)?;
    app.supervisor.reinstall(id).map_err(refused)?;
    let row = app.store.get_server(id)?.unwrap_or(row);
    Ok(Json(server_view(&app, &row)))
}

#[derive(Deserialize)]
struct ConsoleQuery {
    #[serde(default = "default_tail")]
    tail: usize,
    #[serde(default)]
    follow: bool,
}

fn default_tail() -> usize {
    200
}

async fn console(
    State(app): S,
    caller: Caller,
    Path(id): Path<u64>,
    Query(q): Query<ConsoleQuery>,
) -> ApiResult<Response> {
    load(&app, &caller, id)?;
    let inst = app.supervisor.instance(id);
    let (past, rx) = inst.console.follow(q.tail.min(2000));
    let mut first = past.join("\n");
    if !first.is_empty() {
        first.push('\n');
    }
    let head = futures_util::stream::once(async move { Ok::<_, std::io::Error>(first) });
    let body = if q.follow {
        let live = futures_util::stream::unfold(rx, |mut rx| async move {
            loop {
                match rx.recv().await {
                    Ok(line) => return Some((Ok(format!("{line}\n")), rx)),
                    // Too slow a reader: it misses some lines.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        });
        Body::from_stream(futures_util::StreamExt::chain(head, live))
    } else {
        Body::from_stream(head)
    };
    Ok((
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response())
}

#[derive(Deserialize)]
struct LinesQuery {
    #[serde(default)]
    after: u64,
    #[serde(default)]
    wait: u64,
}

/// The console for a page that cannot stream (the desktop window): lines after
/// `after`, waiting up to `wait` seconds for some.
async fn console_lines(
    State(app): S,
    caller: Caller,
    Path(id): Path<u64>,
    Query(q): Query<LinesQuery>,
) -> ApiResult<Json<ConsoleLines>> {
    load(&app, &caller, id)?;
    let inst = app.supervisor.instance(id);
    let wait = Duration::from_secs(q.wait.min(25));
    let (lines, next, reset) = inst.console.wait_since(q.after, wait).await;
    Ok(Json(ConsoleLines { lines, next, reset }))
}

async fn command(
    State(app): S,
    caller: Caller,
    Path(id): Path<u64>,
    Json(req): Json<CommandRequest>,
) -> ApiResult<StatusCode> {
    load(&app, &caller, id)?;
    app.supervisor.send(id, req.line.trim()).map_err(refused)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_files(
    State(app): S,
    caller: Caller,
    Path(id): Path<u64>,
) -> ApiResult<Json<Vec<FileView>>> {
    let row = load(&app, &caller, id)?;
    let game = app
        .games
        .get(&row.game)
        .ok_or_else(|| not_found("archipel no longer knows this server's game"))?;
    let dir = app.cfg.server_dir(id);
    if !dir.exists() {
        fsops::ensure_server_dir(&dir, fsops::uid_of(id)).context("preparing its directory")?;
    }
    Ok(Json(files::editable(game, &dir, fsops::uid_of(id)).await?))
}

#[derive(Deserialize)]
struct FileQuery {
    path: String,
}

async fn save_file(
    State(app): S,
    caller: Caller,
    Path(id): Path<u64>,
    Query(q): Query<FileQuery>,
    Json(req): Json<SaveFile>,
) -> ApiResult<Json<FileView>> {
    let row = load(&app, &caller, id)?;
    let game = app
        .games
        .get(&row.game)
        .ok_or_else(|| not_found("archipel no longer knows this server's game"))?;
    let dir = app.cfg.server_dir(id);
    let uid = fsops::uid_of(id);
    fsops::ensure_server_dir(&dir, uid).context("preparing its directory")?;
    files::save(game, &dir, uid, &q.path, req.content)
        .await
        .map_err(|e| bad(format!("{e:#}")))?;
    files::editable(game, &dir, uid)
        .await?
        .into_iter()
        .find(|f| f.path == q.path)
        .map(Json)
        .ok_or_else(|| not_found("no such file"))
}

// ---------------------------------------------------------------- machine and users

async fn overview(State(app): S, _: Caller) -> ApiResult<Json<Overview>> {
    let rows = app.store.list_servers()?;
    let used = if app.supervisor.hosted() {
        app.supervisor.memory_in_use().1
    } else {
        // No limits on the desktop: what servers take, measured.
        rows.iter()
            .filter_map(|r| app.supervisor.instance(r.id).usage())
            .map(|u| u.memory_bytes / (1024 * 1024))
            .sum()
    };
    let running = rows
        .iter()
        .filter(|r| app.supervisor.instance(r.id).status().state.is_active())
        .count() as u32;
    let (first, last) = app.cfg.port_range()?;
    let cgroups = app.supervisor.cgroups();
    let (_, cpu) = cgroups.totals();
    let (disk_total, disk_used) = disk_space(&app.cfg.data_dir).unzip();
    Ok(Json(Overview {
        memory_mb: app.supervisor.memory_available_mb(),
        cpu_percent: cpu,
        memory_used_mb: used,
        disk_total_bytes: disk_total,
        disk_used_bytes: disk_used,
        limits_enforced: cgroups.enforced(),
        limits_note: cgroups.note(),
        ports_first: first,
        ports_last: last,
        ports_used: rows.iter().map(|r| r.ports.len() as u32).sum(),
        public_host: app.cfg.public_host.clone(),
        servers: rows.len() as u32,
        running,
    }))
}

/// Whether friends could reach this computer (the desktop app).
async fn internet(State(app): S, _: Caller) -> ApiResult<Json<Internet>> {
    let reacher = app
        .supervisor
        .reacher()
        .ok_or_else(|| not_found("only the desktop app opens ports on a router"))?;
    let status = reacher.machine();
    let (first, last) = app.cfg.port_range()?;
    Ok(Json(Internet {
        state: status.state,
        local_ip: status.local_ip.map(|ip| ip.to_string()),
        public_ip: status.public_ip.map(|ip| ip.to_string()),
        ports: format!("{first}-{last}"),
        detail: status.detail,
        firewall: crate::reach::firewall_hint(first, last),
    }))
}

async fn check_internet(State(app): S, caller: Caller) -> ApiResult<Json<Internet>> {
    // It says `checking` until done.
    app.supervisor.check_internet();
    internet(State(app), caller).await
}

/// Total and used bytes of the filesystem holding `path`.
fn disk_space(path: &std::path::Path) -> Option<(u64, u64)> {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
    // SAFETY: statvfs fills the struct it is given.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let block = s.f_frsize as u64;
    let total = s.f_blocks as u64 * block;
    let free = s.f_bavail as u64 * block;
    Some((total, total.saturating_sub(free)))
}

async fn list_users(State(app): S, caller: Caller) -> ApiResult<Json<Vec<UserView>>> {
    caller.admin()?;
    Ok(Json(
        app.store
            .list_users()?
            .iter()
            .map(|u| user_view(&app, u))
            .collect(),
    ))
}

fn check_username(name: &str) -> ApiResult<()> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_.".contains(c));
    if ok {
        Ok(())
    } else {
        Err(bad(
            "usernames are 1 to 32 lowercase letters, digits, - _ and .",
        ))
    }
}

async fn create_user(
    State(app): S,
    caller: Caller,
    Json(req): Json<CreateUser>,
) -> ApiResult<Json<UserView>> {
    caller.admin()?;
    let username = req.username.trim().to_lowercase();
    check_username(&username)?;
    check_password(&req.password)?;
    app.store
        .create_user(
            &username,
            &hash_password(&req.password)?,
            req.role,
            &req.quota,
        )
        .map_err(refused)?;
    let user = app
        .store
        .user_by_name(&username)?
        .context("the user just created is gone")?;
    info!("{} created user {username}", caller.user.username);
    Ok(Json(user_view(&app, &user)))
}

async fn update_user(
    State(app): S,
    caller: Caller,
    Path(username): Path<String>,
    Json(req): Json<UpdateUser>,
) -> ApiResult<Json<UserView>> {
    caller.admin()?;
    let mut user = app
        .store
        .user_by_name(&username)?
        .ok_or_else(|| not_found(format!("no user named {username}")))?;
    if let Some(password) = &req.password {
        check_password(password)?;
        user.password_hash = hash_password(password)?;
    }
    if let Some(role) = req.role {
        if role != Role::Admin && user.role == Role::Admin && admins(&app)? == 1 {
            return Err(bad("there must be an admin left"));
        }
        user.role = role;
    }
    if let Some(quota) = req.quota {
        user.quota = quota;
        app.supervisor.cgroups().ensure_user(user.id, &user.quota)?;
    }
    app.store.update_user(&user)?;
    if req.password.is_some() {
        app.store.delete_sessions_of(user.id, None)?;
    }
    Ok(Json(user_view(&app, &user)))
}

fn admins(app: &App) -> anyhow::Result<usize> {
    Ok(app
        .store
        .list_users()?
        .iter()
        .filter(|u| u.role == Role::Admin)
        .count())
}

async fn delete_user(
    State(app): S,
    caller: Caller,
    Path(username): Path<String>,
) -> ApiResult<StatusCode> {
    caller.admin()?;
    let user = app
        .store
        .user_by_name(&username)?
        .ok_or_else(|| not_found(format!("no user named {username}")))?;
    if user.id == caller.user.id {
        return Err(bad("you cannot delete yourself"));
    }
    let servers = app
        .store
        .list_servers()?
        .into_iter()
        .filter(|r| r.owner_id == user.id)
        .count();
    if servers > 0 {
        return Err(refused(anyhow::anyhow!(
            "{username} still has {servers} servers: delete them first"
        )));
    }
    app.store.delete_user(user.id)?;
    Ok(StatusCode::NO_CONTENT)
}
