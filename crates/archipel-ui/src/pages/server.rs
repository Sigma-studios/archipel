use std::time::Duration;

use archipel_core::api::{
    CommandRequest, FileView, SaveFile, ServerState, ServerView, UpdateServer, UserView,
};
use archipel_core::game::GameDef;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::NavigateOptions;
use leptos_router::hooks::{use_navigate, use_params_map, use_query_map};

use super::servers::badge_of;
use super::settings_form::{MemoryField, SettingsFields, defaults};
use crate::api;
use crate::components::{
    ConnectAddress, ErrorNote, GameBadge, Loading, Modal, PowerButton, StateBadge,
};
use crate::util::{is_admin, mb, polled, run, since, strip_ansi, toasts, use_me};

const TABS: &[(&str, &str)] = &[
    ("console", "Console"),
    ("settings", "Settings"),
    ("files", "Files"),
    ("delete", "Delete"),
];

#[component]
pub fn ServerPage() -> impl IntoView {
    let params = use_params_map();
    let query = use_query_map();
    let navigate = use_navigate();
    let me = use_me();
    let id = Memo::new(move |_| {
        params
            .read()
            .get("id")
            .and_then(|i| i.parse::<u64>().ok())
            .unwrap_or(0)
    });
    let tab = Memo::new(move |_| query.read().get("tab").unwrap_or_else(|| "console".into()));

    let server = polled(Some(Duration::from_millis(2500)), move || {
        let id = id.get();
        async move { api::get::<ServerView>(&format!("/api/servers/{id}")).await }
    });
    let games = polled(None, || async {
        api::get::<Vec<GameDef>>("/api/games").await
    });
    // The server as last fetched, notifying only when it changed.
    let current = Memo::new(move |_| server.data.get());
    let game = Memo::new(move |_| {
        let id = current.with(|s| s.as_ref().map(|s| s.game.clone()))?;
        games
            .data
            .with(|g| g.iter().flatten().find(|g| g.id == id).cloned())
    });
    let field = move |f: fn(&ServerView) -> String| {
        move || current.with(|s| s.as_ref().map(f).unwrap_or_default())
    };
    let state = Signal::derive(move || {
        current.with(|s| s.as_ref().map_or(ServerState::Stopped, |s| s.state))
    });
    let navigate = StoredValue::new_local(navigate);
    let set_tab = move |t: &str| {
        navigate.with_value(|navigate| {
            navigate(
                &format!("/servers/{}?tab={t}", id.get_untracked()),
                NavigateOptions {
                    replace: true,
                    scroll: false,
                    ..Default::default()
                },
            )
        })
    };
    let on_changed = Callback::new(move |s: ServerView| {
        server.data.set(Some(s));
        me.reload();
    });
    let restart = move |_| {
        let id = id.get_untracked();
        run(
            "Restarting…",
            async move { api::act(&format!("/api/servers/{id}/restart")).await },
            move || server.reload(),
        );
    };
    let subtitle = move || {
        current.with(|s| {
            let Some(s) = s else { return String::new() };
            let mut parts = vec![
                s.game_name.clone(),
                format!("{} of memory", mb(s.memory_mb)),
            ];
            let mine = me
                .data
                .with(|u| u.as_ref().is_some_and(|u| u.username == s.owner));
            if !mine {
                parts.push(format!("owned by {}", s.owner));
            }
            if let (ServerState::Running, Some(at)) = (s.state, s.started_at) {
                parts.push(format!("up for {}", since(at)));
            }
            if let Some(u) = &s.usage {
                parts.push(format!("CPU {:.0} %", u.cpu_percent));
            }
            parts.join(" · ")
        })
    };

    view! {
        <div class="page">
            <a href="/" class="crumb">"← Servers"</a>
            <ErrorNote error=server.error/>
            <Show when=move || server.loaded() fallback=Loading>
                <header class="server-header">
                    <GameBadge size="lg"
                        badge=Signal::derive(move || badge_of(games, &field(|s| s.game.clone())()))
                        game=Signal::derive(field(|s| s.game.clone()))/>
                    <div class="grow">
                        <h1 class="title-row">
                            {field(|s| s.name.clone())}
                            <StateBadge state/>
                        </h1>
                        <div class="server-header-sub">
                            <span>{subtitle}</span>
                        </div>
                    </div>
                    <div class="actions">
                        <ConnectAddress template=Signal::derive(move || current.with(|s| s.as_ref().and_then(|s| s.connect.clone())))/>
                        <PowerButton big=true on_done=on_changed
                            server=Signal::derive(move || current.get().expect("loaded"))/>
                        <button type="button" class="button button-big"
                            disabled=move || state.get() != ServerState::Running
                            on:click=restart>"↻ Restart"</button>
                    </div>
                </header>

                {move || current.with(|s| s.as_ref().and_then(|s| s.message.clone().map(|m| (s.state, m))))
                    .filter(|(st, _)| matches!(st, ServerState::Crashed | ServerState::Installing | ServerState::Starting))
                    .map(|(st, m)| view! {
                        <div class=if st == ServerState::Crashed { "note note-error" } else { "note note-info" }
                            role="status">
                            <strong>{match st {
                                ServerState::Crashed => "It stopped unexpectedly: ",
                                ServerState::Installing => "Getting the game ready: ",
                                _ => "",
                            }}</strong>
                            {m}
                        </div>
                    })}
                <Show when=move || state.get() == ServerState::Installing && current.with(|s| s.as_ref().is_some_and(|s| s.message.is_none()))>
                    <div class="note note-info" role="status">
                        "Downloading the game files. The first time takes a few minutes; the server starts right after."
                    </div>
                </Show>

                <div class="tabs" role="tablist" aria-label="Server">
                    {TABS.iter().map(|(key, label)| {
                        let key = *key;
                        let danger = if key == "delete" { " tab-danger" } else { "" };
                        view! {
                            <button type="button" role="tab" id=format!("tab-{key}")
                                class=move || if tab.get() == key { format!("tab active{danger}") } else { format!("tab{danger}") }
                                aria-selected=move || (tab.get() == key).to_string()
                                aria-controls="tab-panel"
                                on:click=move |_| set_tab(key)>
                                {*label}
                            </button>
                        }
                    }).collect_view()}
                </div>

                <div role="tabpanel" id="tab-panel" aria-labelledby=move || format!("tab-{}", tab.get())>
                    {move || match tab.get().as_str() {
                        "settings" => view! {
                            <Show when=move || games.loaded() fallback=Loading>
                                {move || match game.get() {
                                    Some(game) => view! {
                                        <SettingsTab server=server.data.get_untracked().expect("loaded")
                                            game live=current on_saved=on_changed/>
                                    }.into_any(),
                                    None => view! {
                                        <div class="note note-warning">
                                            "This server's game is no longer installed in archipel, so its settings can't be shown."
                                        </div>
                                    }.into_any(),
                                }}
                            </Show>
                        }.into_any(),
                        "files" => view! { <FilesTab id/> }.into_any(),
                        "delete" => view! {
                            <DeleteTab id name=Signal::derive(field(|s| s.name.clone()))/>
                        }.into_any(),
                        _ => view! { <ConsoleTab id server=current/> }.into_any(),
                    }}
                </div>
            </Show>
        </div>
    }
}

// ---------------------------------------------------------------- console

const MAX_LINES: usize = 5000;

#[derive(Clone, Copy, PartialEq)]
enum LineKind {
    Output,
    /// Typed here.
    Command,
}

#[component]
fn ConsoleTab(id: Memo<u64>, server: Memo<Option<ServerView>>) -> impl IntoView {
    let lines = RwSignal::new(Vec::<(usize, LineKind, String)>::new());
    let counter = StoredValue::new(0usize);
    let next_key = move || {
        counter.update_value(|c| *c += 1);
        counter.get_value()
    };
    let streaming = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let retry = RwSignal::new(0u32);
    let controller = StoredValue::new_local(None::<web_sys::AbortController>);
    let pane = NodeRef::<leptos::html::Div>::new();
    // Follow the newest line, unless the user scrolled up to read.
    let stick = RwSignal::new(true);
    let state = move || server.with(|s| s.as_ref().map_or(ServerState::Stopped, |s| s.state));
    // A new run of the server: open the stream again to follow it.
    let epoch = Memo::new(move |_| {
        server.with(|s| s.as_ref().map(|s| (s.started_at, s.state.is_active())))
    });

    Effect::new(move |_| {
        let id = id.get();
        epoch.track();
        retry.track();
        if let Some(previous) = controller.get_value() {
            previous.abort();
        }
        let Ok(abort) = web_sys::AbortController::new() else {
            return;
        };
        controller.set_value(Some(abort.clone()));
        error.set(None);
        streaming.set(true);
        spawn_local(async move {
            let mut partial = String::new();
            // The stream starts with the last lines again: they replace what is shown.
            let mut first = true;
            let path = format!("/api/servers/{id}/console?tail=500&follow=true");
            let result = api::stream_text(&path, &abort.signal(), |chunk| {
                partial.push_str(&chunk);
                let mut fresh = vec![];
                while let Some(i) = partial.find('\n') {
                    let line: String = partial.drain(..=i).collect();
                    fresh.push((next_key(), LineKind::Output, strip_ansi(line.trim_end())));
                }
                if !fresh.is_empty() {
                    let replace = std::mem::take(&mut first);
                    lines.try_update(|l| {
                        if replace {
                            l.clear();
                        }
                        l.extend(fresh);
                        if l.len() > MAX_LINES {
                            l.drain(..l.len() - MAX_LINES);
                        }
                    });
                }
            })
            .await;
            if abort.signal().aborted() {
                return;
            }
            streaming.try_set(false);
            if let Err(e) = result {
                error.try_set(Some(e));
            }
            // The stream ended while the server should be running: try again shortly.
            let active = server
                .try_get_untracked()
                .flatten()
                .is_some_and(|s| s.state.is_active());
            if active {
                set_timeout(
                    move || {
                        retry.try_update(|r| *r += 1);
                    },
                    Duration::from_secs(3),
                );
            }
        });
    });
    on_cleanup(move || {
        if let Some(c) = controller.try_get_value().flatten() {
            c.abort();
        }
    });
    let to_bottom = move || {
        if let Some(el) = pane.get_untracked() {
            el.set_scroll_top(el.scroll_height());
        }
    };
    Effect::new(move |_| {
        lines.track();
        if stick.get_untracked() {
            to_bottom();
        }
    });
    let on_scroll = move |_| {
        if let Some(el) = pane.get_untracked() {
            let at_bottom = el.scroll_top() + el.client_height() >= el.scroll_height() - 24;
            stick.set(at_bottom);
        }
    };

    // The command line, with its history on the up and down arrows.
    let command = RwSignal::new(String::new());
    let history = StoredValue::new(Vec::<String>::new());
    let cursor = StoredValue::new(None::<usize>);
    let input = NodeRef::<leptos::html::Input>::new();
    let can_type = move || matches!(state(), ServerState::Running | ServerState::Starting);
    let send = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let line = command.get_untracked().trim().to_string();
        if line.is_empty() {
            return;
        }
        history.update_value(|h| {
            if h.last() != Some(&line) {
                h.push(line.clone());
            }
            if h.len() > 200 {
                h.remove(0);
            }
        });
        cursor.set_value(None);
        command.set(String::new());
        stick.set(true);
        lines.update(|l| l.push((next_key(), LineKind::Command, line.clone())));
        let id = id.get_untracked();
        spawn_local(async move {
            let body = CommandRequest { line };
            if let Err(e) = api::post_empty(&format!("/api/servers/{id}/console"), &body).await {
                toasts().err(e);
            }
        });
    };
    let on_key = move |ev: leptos::ev::KeyboardEvent| {
        let len = history.with_value(Vec::len);
        if len == 0 {
            return;
        }
        match ev.key().as_str() {
            "ArrowUp" => {
                ev.prevent_default();
                let i = cursor.get_value().map_or(len - 1, |i| i.saturating_sub(1));
                cursor.set_value(Some(i));
                command.set(history.with_value(|h| h[i].clone()));
            }
            "ArrowDown" => {
                let Some(i) = cursor.get_value() else {
                    return;
                };
                ev.prevent_default();
                if i + 1 < len {
                    cursor.set_value(Some(i + 1));
                    command.set(history.with_value(|h| h[i + 1].clone()));
                } else {
                    cursor.set_value(None);
                    command.set(String::new());
                }
            }
            _ => {}
        }
    };

    view! {
        <ErrorNote error=error/>
        <section class="console" aria-label="Console">
            <div class="console-bar">
                <span class=move || if streaming.get() { "console-live on" } else { "console-live" }>
                    {move || match (streaming.get(), state()) {
                        (true, _) => "Live",
                        (false, ServerState::Stopped | ServerState::Crashed) => "Server stopped",
                        (false, _) => "Reconnecting…",
                    }}
                </span>
                <span class="push-right">
                    <button type="button" class="icon-button" title="Clear the console view (the server keeps its log)"
                        on:click=move |_| lines.set(vec![])>"Clear"</button>
                </span>
            </div>
            <div class="logs" node_ref=pane on:scroll=on_scroll role="log" aria-live="off" tabindex="0"
                aria-label="Server output">
                <Show when=move || lines.with(|l| !l.is_empty()) fallback=move || view! {
                    <div class="logs-empty">{move || match state() {
                        ServerState::Stopped => "Nothing yet. Start the server and its output appears here.",
                        ServerState::Crashed => "No output. Start the server again to see what it says.",
                        _ => "Waiting for the server to say something…",
                    }}</div>
                }>
                    <For each=move || lines.get() key=|(i, _, _)| *i let:line>
                        <LogLine kind=line.1 text=line.2/>
                    </For>
                </Show>
            </div>
            <Show when=move || !stick.get()>
                <button type="button" class="jump-latest" on:click=move |_| {
                    stick.set(true);
                    to_bottom();
                }>"↓ Latest"</button>
            </Show>
            <form class="console-input" on:submit=send>
                <span class="console-prompt" aria-hidden="true">">"</span>
                <label for="console-command" class="visually-hidden">"Console command"</label>
                <input type="text" id="console-command" node_ref=input bind:value=command
                    autocomplete="off" autocapitalize="off" spellcheck="false"
                    disabled=move || !can_type() on:keydown=on_key
                    placeholder=move || if can_type() {
                        "Type a command and press Enter (↑ for the previous ones)"
                    } else {
                        "Start the server to type commands"
                    }/>
                <button type="submit" class="button button-small"
                    disabled=move || !can_type() || command.with(|c| c.trim().is_empty())>"Send"</button>
            </form>
        </section>
    }
}

#[component]
fn LogLine(kind: LineKind, text: String) -> impl IntoView {
    let class = match kind {
        LineKind::Command => "log-line log-command",
        LineKind::Output => {
            let upper = text.to_ascii_uppercase();
            if upper.contains("ERROR") || upper.contains("SEVERE") || upper.contains("EXCEPTION") {
                "log-line log-error"
            } else if upper.contains("WARN") {
                "log-line log-warn"
            } else {
                "log-line"
            }
        }
    };
    let text = match kind {
        LineKind::Command => format!("> {text}"),
        LineKind::Output => text,
    };
    view! { <div class=class>{text}</div> }
}

// ---------------------------------------------------------------- settings

#[component]
fn SettingsTab(
    /// As it was when the tab opened: the form starts from it.
    server: ServerView,
    game: GameDef,
    /// As it is now.
    live: Memo<Option<ServerView>>,
    on_saved: Callback<ServerView>,
) -> impl IntoView {
    let me = use_me();
    let owner = server.owner.clone();
    // Someone else's server, seen by an admin: their quota comes from the users list.
    let others = Memo::new({
        let owner = owner.clone();
        move |_| {
            is_admin(me)
                && me
                    .data
                    .with(|u| u.as_ref().is_some_and(|u| u.username != owner))
        }
    });
    let users = polled(None, {
        move || {
            let fetch = others.get();
            async move {
                if fetch {
                    api::get::<Vec<UserView>>("/api/users").await
                } else {
                    Ok(vec![])
                }
            }
        }
    });
    let id = server.id;
    let min_mb = game.memory.min_mb;
    let current_mb = server.memory_mb;
    let default_mb = game.memory.default_mb;
    // Up to the owner's whole quota: it is checked again when the server starts.
    let max_mb = Signal::derive({
        let owner = owner.clone();
        move || {
            let quota = me
                .data
                .with(|u| {
                    u.as_ref()
                        .filter(|u| u.username == owner)
                        .map(|u| u.quota.memory_mb)
                })
                .or_else(|| {
                    users.data.with(|l| {
                        l.iter()
                            .flatten()
                            .find(|u| u.username == owner)
                            .map(|u| u.quota.memory_mb)
                    })
                })
                .unwrap_or(current_mb);
            quota.max(current_mb).max(min_mb)
        }
    });

    let name = RwSignal::new(server.name.clone());
    let memory = RwSignal::new(server.memory_mb);
    let mut initial = defaults(&game);
    initial.extend(server.settings.clone());
    let values = RwSignal::new(initial);
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let saved = RwSignal::new(false);
    let running =
        move || live.with(|s| s.as_ref().is_some_and(|s| s.state == ServerState::Running));
    let stopped = move || {
        live.with(|s| {
            s.as_ref()
                .is_some_and(|s| matches!(s.state, ServerState::Stopped | ServerState::Crashed))
        })
    };
    let game_name = game.name.clone();
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
        let body = UpdateServer {
            name: Some(server_name),
            memory_mb: Some(memory.get_untracked()),
            settings: Some(settings),
        };
        error.set(None);
        busy.set(true);
        spawn_local(async move {
            match api::put::<_, ServerView>(&format!("/api/servers/{id}"), &body).await {
                Ok(s) => {
                    toasts().ok("Settings saved");
                    saved.try_set(true);
                    on_saved.run(s);
                }
                Err(e) => {
                    error.try_set(Some(e));
                }
            }
            busy.try_set(false);
        });
    };
    let restart = move |_| {
        run(
            "Restarting with the new settings…",
            async move { api::act(&format!("/api/servers/{id}/restart")).await },
            move || saved.set(false),
        );
    };
    let reinstall = move |_| {
        run(
            "Downloading the game files again…",
            async move { api::act(&format!("/api/servers/{id}/reinstall")).await },
            || {},
        );
    };

    view! {
        <form class="card form" on:submit=submit novalidate>
            <fieldset class="form-section">
                <legend>"Basics"</legend>
                <div class="form-grid">
                    <label class="field">
                        <span class="field-label">"Server name" <span class="required" aria-hidden="true">"*"</span></span>
                        <input type="text" bind:value=name required maxlength="64" autocomplete="off"/>
                    </label>
                    <MemoryField value=memory min=min_mb max=max_mb default_mb
                        help=Signal::derive(|| "Taken from your quota while the server runs.".to_string())/>
                </div>
            </fieldset>
            <fieldset class="form-section">
                <legend>"Game settings"</legend>
                <SettingsFields game=game.get_value() values editing=true/>
            </fieldset>
            <ErrorNote error=error/>
            <Show when=move || saved.get() && running()>
                <div class="note note-info" role="status">
                    "Saved. The server is running with the old settings until it restarts. "
                    <button type="button" class="button button-small" on:click=restart>"↻ Restart now"</button>
                </div>
            </Show>
            <div class="form-actions">
                <span class="muted small">"Changes apply at the next start."</span>
                <button type="submit" class="button button-primary" disabled=busy>
                    {move || if busy.get() { "Saving…" } else { "Save settings" }}
                </button>
            </div>
        </form>
        <section class="card danger-zone">
            <div>
                <h2 class="section-title">"Download the game files again"</h2>
                <p class="muted small">
                    {format!("Gets a fresh copy of {game_name}'s files, to update it or repair a broken install. ")}
                    <Show when=move || !stopped()>"Stop the server first."</Show>
                </p>
            </div>
            <button type="button" class="button" disabled=move || !stopped() on:click=reinstall>"Reinstall"</button>
        </section>
    }
}

// ---------------------------------------------------------------- files

#[component]
fn FilesTab(id: Memo<u64>) -> impl IntoView {
    let files = polled(None, move || {
        let id = id.get();
        async move { api::get::<Vec<FileView>>(&format!("/api/servers/{id}/files")).await }
    });
    let selected = RwSignal::new(None::<String>);
    let content = RwSignal::new(String::new());
    let original = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let dirty = move || content.with(|c| original.with(|o| c != o));
    let open = move |f: FileView| {
        selected.set(Some(f.path));
        content.set(f.content.clone());
        original.set(f.content);
    };
    // Open the first file once the list is there.
    Effect::new(move |_| {
        let first = files
            .data
            .with(|f| f.as_ref().and_then(|f| f.first().cloned()));
        if selected.with_untracked(Option::is_none)
            && let Some(f) = first
        {
            open(f);
        }
    });
    let file = Memo::new(move |_| {
        let path = selected.get()?;
        files
            .data
            .with(|f| f.iter().flatten().find(|f| f.path == path).cloned())
    });
    let choose = move |f: FileView| {
        if selected.with_untracked(|s| s.as_deref() == Some(f.path.as_str())) {
            return;
        }
        if dirty()
            && !window()
                .confirm_with_message("Your changes to this file are not saved. Discard them?")
                .unwrap_or(false)
        {
            return;
        }
        open(f);
    };
    let save = move || {
        let Some(f) = file.get_untracked() else {
            return;
        };
        let id = id.get_untracked();
        let path = String::from(js_sys::encode_uri_component(&f.path));
        let body = SaveFile {
            content: content.get_untracked(),
        };
        busy.set(true);
        spawn_local(async move {
            match api::put::<_, FileView>(&format!("/api/servers/{id}/files?path={path}"), &body)
                .await
            {
                Ok(saved) => {
                    toasts().ok(format!(
                        "{} saved. It is used at the next start.",
                        label_of(&saved)
                    ));
                    original.try_set(saved.content.clone());
                    content.try_set(saved.content);
                    files.reload();
                }
                Err(e) => toasts().err(e),
            }
            busy.try_set(false);
        });
    };
    let on_key = move |ev: leptos::ev::KeyboardEvent| {
        if (ev.ctrl_key() || ev.meta_key()) && ev.key().eq_ignore_ascii_case("s") {
            ev.prevent_default();
            save();
        }
    };

    view! {
        <ErrorNote error=files.error/>
        <Show when=move || files.loaded() fallback=Loading>
            <Show when=move || files.data.with(|f| f.as_ref().is_some_and(|f| !f.is_empty())) fallback=|| view! {
                <div class="empty">
                    <div class="empty-art" aria-hidden="true">"📄"</div>
                    <h2>"No files to edit"</h2>
                    <p>"Everything this game can change is in the Settings tab."</p>
                </div>
            }>
                <div class="split">
                    <ul class="file-list" aria-label="Files">
                        <For each=move || files.data.get().unwrap_or_default() key=|f| f.path.clone() let:f>
                            {
                                let path = StoredValue::new(f.path.clone());
                                let active = move || {
                                    selected.with(|s| path.with_value(|p| s.as_deref() == Some(p.as_str())))
                                };
                                let label = label_of(&f);
                                let shown_path = f.path.clone();
                                view! {
                                    <li>
                                        <button type="button" class=move || if active() { "file-item active" } else { "file-item" }
                                            aria-current=move || active().then_some("true")
                                            on:click=move |_| choose(f.clone())>
                                            <span class="file-label">{label}</span>
                                            <span class="file-path">{shown_path}</span>
                                        </button>
                                    </li>
                                }
                            }
                        </For>
                    </ul>
                    {move || file.get().map(|f| {
                        let label = label_of(&f);
                        view! {
                            <section class="card editor-card" aria-label=label.clone()>
                                <div class="card-head">
                                    <div>
                                        <h3>{label.clone()}</h3>
                                        <div class="file-path">{f.path.clone()}</div>
                                    </div>
                                    <div class="actions">
                                        <Show when=dirty>
                                            <span class="badge badge-busy">"Unsaved changes"</span>
                                        </Show>
                                    </div>
                                </div>
                                {(!f.managed_keys.is_empty()).then(|| view! {
                                    <div class="note note-info">
                                        "These keys are set from the Settings tab at each start, so changing them here does nothing:"
                                        <div class="managed-keys">
                                            {f.managed_keys.iter().map(|k| view! { <code>{k.clone()}</code> }).collect_view()}
                                        </div>
                                    </div>
                                })}
                                {f.content.is_empty().then(|| view! {
                                    <div class="note">"This file doesn't exist yet: archipel writes it at the server's first start. What you save here is used instead."</div>
                                })}
                                <textarea class="editor" aria-label=format!("Contents of {label}")
                                    spellcheck="false" autocapitalize="off" autocomplete="off" wrap="off"
                                    prop:value=move || content.get()
                                    on:input=move |ev| content.set(event_target_value(&ev))
                                    on:keydown=on_key></textarea>
                                <div class="form-actions">
                                    <span class="muted small">"Changes apply at the next start. Ctrl+S saves."</span>
                                    <button type="button" class="button" disabled=move || !dirty()
                                        on:click=move |_| content.set(original.get_untracked())>"Undo changes"</button>
                                    <button type="button" class="button button-primary"
                                        disabled=move || busy.get() || !dirty()
                                        on:click=move |_| save()>
                                        {move || if busy.get() { "Saving…" } else { "Save file" }}
                                    </button>
                                </div>
                            </section>
                        }
                    })}
                </div>
            </Show>
        </Show>
    }
}

fn label_of(f: &FileView) -> String {
    if f.label.is_empty() {
        f.path.clone()
    } else {
        f.label.clone()
    }
}

// ---------------------------------------------------------------- delete

#[component]
fn DeleteTab(id: Memo<u64>, #[prop(into)] name: Signal<String>) -> impl IntoView {
    let me = use_me();
    let navigate = use_navigate();
    let confirm = RwSignal::new(false);
    let typed = RwSignal::new(String::new());
    let delete = move |_| {
        let navigate = navigate.clone();
        let id = id.get_untracked();
        run(
            format!("{} deleted", name.get_untracked()),
            async move { api::delete(&format!("/api/servers/{id}")).await },
            move || {
                me.reload();
                navigate("/", Default::default());
            },
        );
    };
    view! {
        <section class="card card-danger danger-zone">
            <div>
                <h2 class="section-title">"Delete this server"</h2>
                <p class="muted">
                    "Stops it and deletes everything it has: its world, maps, files and settings. This cannot be undone."
                </p>
            </div>
            <button type="button" class="button button-danger" on:click=move |_| {
                typed.set(String::new());
                confirm.set(true);
            }>"Delete server…"</button>
        </section>
        <Modal open=confirm title="Delete this server?">
            <p>
                "Everything in " <strong>{move || name.get()}</strong>
                " is deleted for good, worlds included. Players won't be able to join it anymore."
            </p>
            <label class="field">
                <span class="field-label">"Type " <code>{move || name.get()}</code> " to confirm"</span>
                <input type="text" bind:value=typed autocomplete="off" autofocus/>
            </label>
            <div class="form-actions">
                <button type="button" class="button" on:click=move |_| confirm.set(false)>"Keep it"</button>
                <button type="button" class="button button-danger"
                    disabled=move || typed.get().trim() != name.get()
                    on:click=delete.clone()>"Delete forever"</button>
            </div>
        </Modal>
    }
}
