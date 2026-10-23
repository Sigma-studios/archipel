use archipel_core::api::{ServerState, ServerView, UserView};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::util::{copy_to_clipboard, cores, mb, percent, plural, tint, toasts};

/// The archipel mark: three islands.
#[component]
pub fn BrandMark() -> impl IntoView {
    view! {
        <svg class="brand-mark" viewBox="0 0 32 32" aria-hidden="true">
            <circle cx="11" cy="19" r="8" fill="#e8590c"/>
            <circle cx="23" cy="10" r="6" fill="#0d9488"/>
            <circle cx="24.5" cy="24.5" r="4" fill="#f59f00"/>
        </svg>
    }
}

/// What a state looks like: badge class, icon, label. The colour only reinforces
/// the icon and the word.
pub fn state_parts(state: ServerState) -> (&'static str, &'static str, &'static str) {
    match state {
        ServerState::Running => ("good", "●", "running"),
        ServerState::Starting => ("busy", "◐", "starting"),
        ServerState::Installing => ("info", "↓", "installing"),
        ServerState::Stopping => ("busy", "◑", "stopping"),
        ServerState::Stopped => ("neutral", "■", "stopped"),
        ServerState::Crashed => ("critical", "✕", "crashed"),
    }
}

#[component]
pub fn StateBadge(#[prop(into)] state: Signal<ServerState>) -> impl IntoView {
    let parts = move || state_parts(state.get());
    view! {
        <span class=move || format!("badge badge-{}", parts().0)>
            <span class="badge-icon" aria-hidden="true">{move || parts().1}</span>
            {move || parts().2}
        </span>
    }
}

/// A game's emoji on a tile tinted after the game.
#[component]
pub fn GameBadge(
    #[prop(into)] badge: Signal<String>,
    #[prop(into)] game: Signal<String>,
    /// `sm`, `lg`, or empty for the default size.
    #[prop(optional)]
    size: &'static str,
) -> impl IntoView {
    let class = move || {
        let size = if size.is_empty() {
            String::new()
        } else {
            format!(" game-badge-{size}")
        };
        format!("game-badge {}{size}", tint(&game.get()))
    };
    view! {
        <span class=class aria-hidden="true">
            {move || {
                let b = badge.get();
                if b.is_empty() { "🎮".to_string() } else { b }
            }}
        </span>
    }
}

/// A usage bar: label, value text, and a fill that turns critical (with an icon)
/// from 90 %.
#[component]
pub fn Meter(
    label: &'static str,
    #[prop(into)] percent: Signal<f64>,
    #[prop(into)] detail: Signal<String>,
    #[prop(optional, into)] sub: Signal<String>,
) -> impl IntoView {
    let high = move || percent.get() >= 90.0;
    view! {
        <div class="meter">
            <div class="meter-head">
                <span class="meter-label">{label}</span>
                <span class="meter-value">
                    <Show when=high>
                        <span class="meter-warn" title="above 90 %">"⚠ "</span>
                    </Show>
                    {move || detail.get()}
                </span>
            </div>
            <div class="meter-track" role="meter" aria-label=label
                aria-valuenow=move || format!("{:.0}", percent.get())
                aria-valuemin="0" aria-valuemax="100"
                aria-valuetext=move || detail.get()>
                <div
                    class=move || if high() { "meter-fill meter-fill-high" } else { "meter-fill" }
                    style=move || format!("width: {:.1}%", percent.get().clamp(0.0, 100.0))
                ></div>
            </div>
            <Show when=move || sub.with(|s| !s.is_empty())>
                <div class="meter-sub">{move || sub.get()}</div>
            </Show>
        </div>
    }
}

/// A user's quota usage: memory, servers and disk, as meters.
#[component]
pub fn QuotaMeters(#[prop(into)] user: Signal<UserView>) -> impl IntoView {
    let q = move || user.get().quota;
    let u = move || user.get().usage;
    view! {
        <div class="meters-row">
            <Meter label="Memory"
                percent=Signal::derive(move || percent(u().memory_mb, q().memory_mb))
                detail=Signal::derive(move || format!("{} of {}", mb(u().memory_mb), mb(q().memory_mb)))
                sub=Signal::derive(move || format!(
                    "Taken by your running servers. CPU: up to {}.", cores(q().cpu_percent)
                ))/>
            <Meter label="Servers"
                percent=Signal::derive(move || percent(u().servers as u64, q().max_servers as u64))
                detail=Signal::derive(move || format!("{} of {}", u().servers, q().max_servers))
                sub=Signal::derive(move || format!("{} running", plural(u().running as u64, "server", "servers")))/>
            <Meter label="Disk"
                percent=Signal::derive(move || percent(u().disk_mb, q().disk_mb))
                detail=Signal::derive(move || format!("{} of {}", mb(u().disk_mb), mb(q().disk_mb)))
                sub=Signal::derive(|| "Worlds, maps and game files.".to_string())/>
        </div>
    }
}

#[component]
pub fn Modal(
    #[prop(into)] open: RwSignal<bool>,
    title: &'static str,
    children: ChildrenFn,
) -> impl IntoView {
    let children = StoredValue::new(children);
    // Move focus into the dialog when it opens.
    let dialog = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        use wasm_bindgen::JsCast;
        if open.get()
            && let Some(el) = dialog.get()
            && let Ok(Some(first)) =
                el.query_selector("input, select, textarea, .form-actions button")
            && let Ok(first) = first.dyn_into::<web_sys::HtmlElement>()
        {
            let _ = first.focus();
        }
    });
    let on_key = move |ev: leptos::ev::KeyboardEvent| {
        if ev.key() == "Escape" {
            open.set(false);
        }
    };
    view! {
        <Show when=move || open.get()>
            <div class="modal-backdrop" on:click=move |_| open.set(false)>
                <div class="modal" role="dialog" aria-modal="true" aria-label=title node_ref=dialog
                    on:click=|ev| ev.stop_propagation() on:keydown=on_key>
                    <div class="modal-head">
                        <h2>{title}</h2>
                        <button type="button" class="icon-button" aria-label="Close"
                            on:click=move |_| open.set(false)>"✕"</button>
                    </div>
                    {children.read_value()()}
                </div>
            </div>
        </Show>
    }
}

/// How players join a server, with a copy button. `template` may hold `{host}`.
#[component]
pub fn ConnectAddress(#[prop(into)] template: Signal<Option<String>>) -> impl IntoView {
    let address = move || template.get().map(|t| crate::util::connect_address(&t));
    view! {
        {move || match address() {
            Some(address) => {
                let copied = address.clone();
                view! {
                    <div class="connect">
                        <code title="How players join">{address}</code>
                        <button type="button" aria-label="Copy the address" on:click=move |ev| {
                            ev.stop_propagation();
                            copy_to_clipboard(&copied);
                            toasts().ok("Address copied: paste it in the game");
                        }>"Copy"</button>
                    </div>
                }.into_any()
            }
            None => view! { <span class="connect-pending">"No address yet"</span> }.into_any(),
        }}
    }
}

/// Start or Stop, whichever the server's state calls for. `on_done` runs once
/// the request went through (with the server as it now is).
#[component]
pub fn PowerButton(
    #[prop(into)] server: Signal<ServerView>,
    on_done: Callback<ServerView>,
    #[prop(optional)] big: bool,
) -> impl IntoView {
    let busy = RwSignal::new(false);
    let state = move || server.with(|s| s.state);
    let act = move |action: &'static str| {
        let id = server.with_untracked(|s| s.id);
        let name = server.with_untracked(|s| s.name.clone());
        busy.set(true);
        spawn_local(async move {
            match api::act_json::<ServerView>(&format!("/api/servers/{id}/{action}")).await {
                Ok(s) => {
                    toasts().ok(match action {
                        "start" => format!("Starting {name}…"),
                        _ => format!("Stopping {name}…"),
                    });
                    on_done.run(s);
                }
                Err(e) => toasts().err(e),
            }
            busy.try_set(false);
        });
    };
    let size = if big { " button-big" } else { "" };
    view! {
        {move || match state() {
            ServerState::Stopped | ServerState::Crashed => view! {
                <button type="button" class=format!("button button-go{size}") disabled=busy
                    on:click=move |ev| { ev.stop_propagation(); act("start") }>
                    <span class="button-icon" aria-hidden="true">"▶"</span>
                    {move || if busy.get() { "Starting…" } else { "Start" }}
                </button>
            }.into_any(),
            ServerState::Stopping => view! {
                <button type="button" class=format!("button{size}") disabled=true>"Stopping…"</button>
            }.into_any(),
            _ => view! {
                <button type="button" class=format!("button{size}") disabled=busy
                    on:click=move |ev| { ev.stop_propagation(); act("stop") }>
                    <span class="button-icon" aria-hidden="true">"■"</span>
                    {move || if busy.get() { "Stopping…" } else { "Stop" }}
                </button>
            }.into_any(),
        }}
    }
}

#[component]
pub fn ErrorNote(#[prop(into)] error: Signal<Option<String>>) -> impl IntoView {
    view! {
        <Show when=move || error.with(Option::is_some)>
            <div class="note note-error" role="alert">
                <span aria-hidden="true">"⚠ "</span>
                {move || error.get().unwrap_or_default()}
            </div>
        </Show>
    }
}

#[component]
pub fn Loading() -> impl IntoView {
    view! {
        <div class="loading" role="status">
            <span class="spinner" aria-hidden="true"></span>
            "Loading…"
        </div>
    }
}
