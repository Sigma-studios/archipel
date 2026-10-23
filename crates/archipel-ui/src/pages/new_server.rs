use archipel_core::api::{CreateServer, ServerView, UserView};
use archipel_core::game::GameDef;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_query_map};

use super::settings_form::{MemoryField, SettingsFields, defaults};
use crate::api;
use crate::components::{ErrorNote, GameBadge, Loading};
use crate::util::{is_admin, mb, polled, toasts, use_me};

#[component]
pub fn NewServer() -> impl IntoView {
    let query = use_query_map();
    let games = polled(None, || async {
        api::get::<Vec<GameDef>>("/api/games").await
    });
    let game = Memo::new(move |_| {
        let id = query.read().get("game")?;
        games
            .data
            .with(|g| g.iter().flatten().find(|g| g.id == id).cloned())
    });
    let step = move |n: u8| {
        let chosen = game.with(Option::is_some);
        match (n, chosen) {
            (1, false) | (2, true) => "step active",
            (1, true) => "step done",
            _ => "step",
        }
    };

    view! {
        <div class="page">
            <header class="page-head">
                <div>
                    <a href="/" class="crumb">"← Servers"</a>
                    <h1>"New server"</h1>
                </div>
            </header>
            <ol class="steps" aria-label="Steps">
                <li class=move || step(1) aria-current=move || game.with(Option::is_none).then_some("step")>
                    <span class="step-num">{move || if game.with(Option::is_some) { "✓" } else { "1" }}</span>
                    "Pick a game"
                </li>
                <li class=move || step(2) aria-current=move || game.with(Option::is_some).then_some("step")>
                    <span class="step-num">"2"</span>
                    "Set it up"
                </li>
            </ol>
            <ErrorNote error=games.error/>
            <Show when=move || games.loaded() fallback=Loading>
                {move || match game.get() {
                    Some(game) => view! { <SetupForm game/> }.into_any(),
                    None => view! {
                        <GamePicker games=Signal::derive(move || games.data.get().unwrap_or_default())/>
                    }.into_any(),
                }}
            </Show>
        </div>
    }
}

#[component]
fn GamePicker(#[prop(into)] games: Signal<Vec<GameDef>>) -> impl IntoView {
    let navigate = use_navigate();
    view! {
        <Show when=move || games.with(|g| !g.is_empty()) fallback=|| view! {
            <div class="empty">
                <div class="empty-art" aria-hidden="true">"🎲"</div>
                <h2>"No games available"</h2>
                <p>"Your admin hasn't added any game to archipel yet."</p>
            </div>
        }>
            <p class="lead">"What do you want to play?"</p>
            <div class="game-grid">
                {
                    let navigate = navigate.clone();
                    move || games.get().into_iter().map(|g| {
                        let navigate = navigate.clone();
                        let href = format!("/servers/new?game={}", g.id);
                        view! {
                            <button type="button" class="game-card" on:click=move |_| navigate(&href, Default::default())>
                                <GameBadge badge=g.badge.clone() game=g.id.clone() size="lg"/>
                                <span>
                                    <span class="game-card-name">{g.name.clone()}</span>
                                    <span class="game-card-desc">{g.description.clone()}</span>
                                    <span class="game-card-meta">{format!("Needs at least {} of memory", mb(g.memory.min_mb))}</span>
                                </span>
                            </button>
                        }
                    }).collect_view()
                }
            </div>
        </Show>
    }
}

#[component]
fn SetupForm(game: GameDef) -> impl IntoView {
    let me = use_me();
    let navigate = use_navigate();
    let admin = Memo::new(move |_| is_admin(me));
    // Admins may create a server for someone else, within that user's quota.
    let users = polled(None, move || {
        let admin = admin.get();
        async move {
            if admin {
                api::get::<Vec<UserView>>("/api/users").await
            } else {
                Ok(vec![])
            }
        }
    });
    let my_name = move || {
        me.data
            .with(|u| u.as_ref().map(|u| u.username.clone()).unwrap_or_default())
    };
    let owner = RwSignal::new(my_name());
    let owner_user = Memo::new(move |_| {
        let owner = owner.get();
        if owner.is_empty() || owner == my_name() {
            return me.data.get();
        }
        users
            .data
            .with(|u| u.iter().flatten().find(|u| u.username == owner).cloned())
    });
    let mine = move || {
        let o = owner.get();
        o.is_empty() || o == my_name()
    };
    let whose = move || {
        if mine() {
            "your".to_string()
        } else {
            format!("{}'s", owner.get())
        }
    };

    let min_mb = game.memory.min_mb;
    let default_mb = game.memory.default_mb;
    let quota_mb = move || owner_user.with(|u| u.as_ref().map_or(0, |u| u.quota.memory_mb));
    let used_mb = move || owner_user.with(|u| u.as_ref().map_or(0, |u| u.usage.memory_mb));
    let free_mb = move || quota_mb().saturating_sub(used_mb());
    let can_start = move || free_mb() >= min_mb;
    let max_mb = Signal::derive(move || if can_start() { free_mb() } else { quota_mb() });
    let too_small = move || owner_user.with(Option::is_some) && quota_mb() < min_mb;
    let full = move || {
        owner_user.with(|u| {
            u.as_ref()
                .is_some_and(|u| u.usage.servers >= u.quota.max_servers)
        })
    };

    let name = RwSignal::new(format!("{} server", game.name));
    let memory = RwSignal::new(default_mb);
    // Keep the memory within what the quota allows as it loads or the owner changes.
    Effect::new(move |_| {
        let hi = max_mb.get().max(min_mb);
        memory.update(|m| *m = (*m).clamp(min_mb, hi));
    });
    let values = RwSignal::new(defaults(&game));
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let game = StoredValue::new(game);

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let server_name = name.get_untracked().trim().to_string();
        if server_name.is_empty() {
            return error.set(Some("Give your server a name.".into()));
        }
        let settings = match game.with_value(|g| g.settings_from(&values.get_untracked())) {
            Ok(s) => s,
            Err(e) => return error.set(Some(e)),
        };
        let start = can_start();
        let owner_name = owner.get_untracked();
        let body = CreateServer {
            name: server_name.clone(),
            game: game.with_value(|g| g.id.clone()),
            memory_mb: memory.get_untracked(),
            settings,
            start,
            owner: (!owner_name.is_empty() && owner_name != my_name()).then_some(owner_name),
        };
        error.set(None);
        busy.set(true);
        let navigate = navigate.clone();
        spawn_local(async move {
            match api::post::<_, ServerView>("/api/servers", &body).await {
                Ok(server) => {
                    toasts().ok(if start {
                        format!("{server_name} is created and starting!")
                    } else {
                        format!("{server_name} is created")
                    });
                    me.reload();
                    navigate(&format!("/servers/{}", server.id), Default::default());
                }
                Err(e) => error.set(Some(e)),
            }
            busy.try_set(false);
        });
    };

    let (badge, id, game_name, description) = game.with_value(|g| {
        (
            g.badge.clone(),
            g.id.clone(),
            g.name.clone(),
            g.description.clone(),
        )
    });
    view! {
        <form class="card form" on:submit=submit novalidate>
            <div class="chosen-game">
                <GameBadge badge=badge game=id size="lg"/>
                <div class="grow">
                    <h2>{game_name}</h2>
                    <div class="muted small">{description}</div>
                </div>
                <a href="/servers/new" class="button button-small">"Change game"</a>
            </div>

            <Show when=full>
                <div class="note note-warning" role="alert">
                    {move || format!(
                        "All {} of {} servers are in use. Delete one to make room, or ask your admin for more.",
                        owner_user.with(|u| u.as_ref().map_or(0, |u| u.quota.max_servers)),
                        whose(),
                    )}
                </div>
            </Show>
            <Show when=too_small>
                <div class="note note-warning" role="alert">
                    {move || format!(
                        "This game needs at least {}, more than {} whole quota ({}). Ask your admin for more memory.",
                        mb(min_mb), whose(), mb(quota_mb()),
                    )}
                </div>
            </Show>

            <fieldset class="form-section">
                <legend>"Basics"</legend>
                <div class="form-grid">
                    <label class="field">
                        <span class="field-label">"Server name" <span class="required" aria-hidden="true">"*"</span></span>
                        <input type="text" bind:value=name required maxlength="64" autocomplete="off"/>
                        <small>"Only for you, to tell your servers apart."</small>
                    </label>
                    <Show when=move || admin.get() && users.data.with(|u| u.as_ref().is_some_and(|u| u.len() > 1))>
                        <label class="field">
                            <span class="field-label">"Owner"</span>
                            <select prop:value=move || owner.get()
                                on:change=move |ev| owner.set(event_target_value(&ev))>
                                {move || users.data.get().unwrap_or_default().into_iter().map(|u| {
                                    let name = u.username.clone();
                                    let label = u.username.clone();
                                    view! {
                                        <option value=u.username selected=move || owner.get() == name>
                                            {label}
                                        </option>
                                    }
                                }).collect_view()}
                            </select>
                            <small>"Their quota applies."</small>
                        </label>
                    </Show>
                    <Show when=move || !too_small()>
                        <MemoryField value=memory min=min_mb max=max_mb default_mb
                            help=Signal::derive(move || if can_start() {
                                format!(
                                    "{} left of {} {} for running servers. More memory lets more players in and loads big worlds faster.",
                                    mb(free_mb()), whose(), mb(quota_mb()),
                                )
                            } else {
                                format!(
                                    "Running servers already use {} of {} {}: stop one to start this one. You can create it now and start it later.",
                                    mb(used_mb()), whose(), mb(quota_mb()),
                                )
                            })/>
                    </Show>
                </div>
            </fieldset>

            <fieldset class="form-section">
                <legend>"Game settings"</legend>
                <SettingsFields game=game.get_value() values/>
                <p class="muted small">"You can change all of these later, from the server's Settings tab."</p>
            </fieldset>

            <ErrorNote error=error/>
            <div class="form-actions">
                <span class="muted small">
                    {move || owner_user.with(|u| u.as_ref().map(|u| format!(
                        "{} of {} servers used",
                        u.usage.servers, u.quota.max_servers,
                    )))}
                </span>
                <a href="/" class="button">"Cancel"</a>
                <button type="submit" class="button button-primary button-big"
                    disabled=move || busy.get() || full() || too_small()>
                    {move || match (busy.get(), can_start()) {
                        (true, _) => "Creating…",
                        (false, true) => "Create and start",
                        (false, false) => "Create",
                    }}
                </button>
            </div>
        </form>
    }
}
