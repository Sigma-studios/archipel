use archipel_core::api::{
    Internet, Outside, Reach, ReachState, ServerState, ServerView, UserView, Verification,
};
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

/// An address players type in their game, with a copy button.
#[component]
pub fn CopyAddress(address: String) -> impl IntoView {
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
    }
}

/// How players join a server, with a copy button. `template` may hold `{host}`.
#[component]
pub fn ConnectAddress(#[prop(into)] template: Signal<Option<String>>) -> impl IntoView {
    // Redrawn only when the address changes, not at each poll of the server.
    let address = Memo::new(move |_| template.get().map(|t| crate::util::connect_address(&t)));
    view! {
        {move || match address.get() {
            Some(address) => view! { <CopyAddress address/> }.into_any(),
            None => view! { <span class="connect-pending">"No address yet"</span> }.into_any(),
        }}
    }
}

/// The host of `host:port`.
fn host_of(address: &str) -> &str {
    address.rsplit_once(':').map_or(address, |(host, _)| host)
}

/// What the last check from outside saw, if one was made.
fn outside(verified: Option<&Verification>) -> Option<Outside> {
    verified.map(|v| v.outside)
}

/// Said under "Friends can join" when no check from outside could be made.
fn unchecked(verified: Option<&Verification>) -> Option<impl IntoView + use<>> {
    let v = verified.filter(|v| !matches!(v.outside, Outside::Reached | Outside::NotReached));
    let why = v
        .and_then(|v| v.detail.clone())
        .map(|d| format!(": {d}"))
        .unwrap_or_default();
    v.map(|_| view! { <p class="small muted">{format!("Not checked from outside{why}.")}</p> })
}

/// Why the hostname set for friends is not the address given, if it is not.
fn host_warning(verified: Option<&Verification>) -> Option<impl IntoView + use<>> {
    verified
        .and_then(|v| v.host_note.clone())
        .map(|note| view! { <p class="small reach-warn">{format!("⚠ {note}.")}</p> })
}

/// Who can join, compactly, for a server's card (desktop app): the address to give
/// and a line saying who it works for.
#[component]
pub fn ReachSummary(#[prop(into)] reach: Signal<Reach>) -> impl IntoView {
    let reach = Memo::new(move |_| reach.get());
    view! {
        {move || {
            let r = reach.get();
            let outside = outside(r.verified.as_ref());
            let address = match r.state {
                // Nothing gets in: the address at home is the one that works.
                ReachState::Internet if outside == Some(Outside::NotReached) => r.local.clone(),
                ReachState::Internet => r.public.clone().or(r.local.clone()),
                _ => r.local.clone(),
            };
            let (class, status) = match r.state {
                ReachState::Checking => ("reach-status muted", "Opening ports on your router…"),
                ReachState::Internet if outside == Some(Outside::NotReached) => {
                    ("reach-status reach-warn", "⚠ Nothing gets through from outside")
                }
                ReachState::Internet if r.firewall.is_some() => {
                    ("reach-status reach-warn", "⚠ This PC's firewall turns friends away")
                }
                ReachState::Internet if outside == Some(Outside::Reached) => {
                    ("reach-status reach-good", "✓ Friends can join (checked from outside)")
                }
                ReachState::Internet => ("reach-status reach-good", "✓ Friends can join"),
                ReachState::LocalOnly => ("reach-status reach-warn", "⚠ Only your home network can join"),
                ReachState::NoPublicAddress => {
                    ("reach-status reach-warn", "⚠ Your connection can't receive games from outside")
                }
            };
            view! {
                <div class="reach">
                    {address.map(|address| view! { <CopyAddress address/> })}
                    <span class=class role="status">
                        {(r.state == ReachState::Checking).then(|| view! {
                            <span class="spinner spinner-sm" aria-hidden="true"></span>
                        })}
                        {status}
                    </span>
                </div>
            }
        }}
    }
}

/// Who can join, in full, for a server's page (desktop app): the address, and how
/// to let friends in when they cannot.
#[component]
pub fn ReachPanel(#[prop(into)] reach: Signal<Reach>) -> impl IntoView {
    let reach = Memo::new(move |_| reach.get());
    view! {
        {move || {
            let r = reach.get();
            let detail = r.detail.clone().map(|d| view! { <div class="small reach-detail">{d}</div> });
            let firewall = r.firewall.clone().map(|f| view! { <p class="reach-firewall">{f}</p> });
            let verified = r.verified.as_ref();
            let this_pc = r
                .local
                .as_deref()
                .map(|l| format!(" ({})", host_of(l)))
                .unwrap_or_default();
            match r.state {
                ReachState::Checking => view! {
                    <div class="note reach-panel" role="status">
                        <span class="spinner spinner-sm" aria-hidden="true"></span>
                        " Opening ports on your router…"
                    </div>
                }.into_any(),
                ReachState::Internet if outside(verified) == Some(Outside::NotReached) => {
                    let why = verified
                        .and_then(|v| v.detail.clone())
                        .map(|d| view! { <div class="small reach-detail">{d}</div> });
                    view! {
                        <div class="note note-warning reach-panel" role="status">
                            <strong>"⚠ Your router says the ports are open, but nothing got through from outside"</strong>
                            {why}
                            <p>
                                {format!(
                                    "Friends can't join until it does. Restarting the router often helps; otherwise forward this server's ports to this PC{this_pc} by hand in the router's settings."
                                )}
                            </p>
                            {firewall}
                            <span>"Players at home can join at:"</span>
                            {r.local.clone().map(|address| view! { <CopyAddress address/> })}
                        </div>
                    }.into_any()
                }
                ReachState::Internet if r.firewall.is_some() => view! {
                    <div class="note note-warning reach-panel" role="status">
                        <strong>"⚠ Your router lets friends in, but this PC's firewall turns them away"</strong>
                        {firewall}
                        <span>"Then give them this address:"</span>
                        {r.public.clone().or(r.local.clone()).map(|address| view! { <CopyAddress address/> })}
                        {host_warning(verified)}
                        {unchecked(verified)}
                    </div>
                }.into_any(),
                ReachState::Internet => view! {
                    <div class="note note-good reach-panel" role="status">
                        <strong>
                            {if outside(verified) == Some(Outside::Reached) {
                                "✓ Friends can join (checked from outside)"
                            } else {
                                "✓ Friends can join"
                            }}
                        </strong>
                        <span>" Give them this address:"</span>
                        {r.public.clone().or(r.local.clone()).map(|address| view! { <CopyAddress address/> })}
                        {host_warning(verified)}
                        {unchecked(verified)}
                        {firewall}
                    </div>
                }.into_any(),
                ReachState::LocalOnly => {
                    let forward = if r.forward.is_empty() {
                        "its ports".to_string()
                    } else {
                        r.forward.join(", ")
                    };
                    view! {
                        <div class="note note-warning reach-panel" role="status">
                            <strong>"⚠ Only your home network can join"</strong>
                            {r.local.clone().map(|address| view! { <CopyAddress address/> })}
                            <p>
                                {format!(
                                    "To let friends in, turn on UPnP in your router's settings, or forward {forward} to this PC{this_pc}."
                                )}
                            </p>
                            {firewall}
                            {detail}
                        </div>
                    }.into_any()
                }
                ReachState::NoPublicAddress => {
                    let local = r.local.clone().unwrap_or_else(|| "this PC's address".into());
                    view! {
                        <div class="note note-warning reach-panel" role="status">
                            <strong>"⚠ Your connection can't receive games from outside"</strong>
                            <p>
                                {format!(
                                    "Your internet provider shares your address with others. Players at home can join at {local}. To let friends in, ask your provider for a full IPv4 address."
                                )}
                            </p>
                            {r.local.clone().map(|address| view! { <CopyAddress address/> })}
                            {detail}
                        </div>
                    }.into_any()
                }
            }
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

/// Whether friends could reach this PC at all (desktop app), on the home page:
/// archipel checks with the router when it opens, before any server runs.
#[component]
pub fn InternetCard() -> impl IntoView {
    let internet = crate::util::polled(Some(std::time::Duration::from_secs(4)), || async {
        api::get::<Internet>("/api/internet").await
    });
    let check = move |_| {
        spawn_local(async move {
            match api::act_json::<Internet>("/api/internet/check").await {
                Ok(i) => internet.data.set(Some(i)),
                Err(e) => toasts().err(e),
            }
        });
    };
    view! {
        <Show when=move || internet.loaded()>
            {move || {
                let i = internet.data.get().expect("loaded");
                let checking = i.state == ReachState::Checking;
                let this_pc = i
                    .local_ip
                    .as_deref()
                    .map(|ip| format!(" ({ip})"))
                    .unwrap_or_default();
                let verified = i.verified.as_ref();
                let not_reached = i.state == ReachState::Internet
                    && outside(verified) == Some(Outside::NotReached);
                // When nothing got through, what the check saw; otherwise the router's.
                let detail = if not_reached {
                    verified.and_then(|v| v.detail.clone())
                } else {
                    i.detail.clone()
                };
                let detail = detail.map(|d| view! { <div class="small reach-detail">{d}</div> });
                let host = verified.and_then(|v| v.host.clone()).filter(|h| h.matches).map(|h| view! {
                    <p>"Friends can use " <strong>{h.name}</strong> " as this PC's address."</p>
                });
                let firewall = i
                    .firewall
                    .clone()
                    .filter(|_| matches!(i.state, ReachState::Internet | ReachState::LocalOnly))
                    .map(|f| view! { <p class="reach-firewall">{f}</p> });
                let (class, title, body) = match i.state {
                    ReachState::Checking => (
                        "note reach-panel",
                        "Checking whether friends can reach this PC…".to_string(),
                        "archipel is asking your router to open a test port.".to_string(),
                    ),
                    ReachState::Internet if not_reached => (
                        "note note-warning reach-panel",
                        "⚠ Your router says the ports are open, but nothing got through from outside".to_string(),
                        format!(
                            "archipel asked a server on the internet to reach this PC on a test port, and it could not. Restarting the router often helps; otherwise forward ports {} (TCP and UDP) to this PC{this_pc} by hand.",
                            i.ports
                        ),
                    ),
                    ReachState::Internet if i.firewall.is_some() => (
                        "note note-warning reach-panel",
                        "⚠ Your router lets friends in, but this PC's firewall turns them away".to_string(),
                        "Open the ports in the firewall (below), then friends can join from the internet.".to_string(),
                    ),
                    ReachState::Internet => {
                        let checked = outside(verified) == Some(Outside::Reached);
                        (
                            "note note-good reach-panel",
                            if checked {
                                "✓ Friends can join from the internet (checked from outside)".to_string()
                            } else {
                                "✓ Friends can join from the internet".to_string()
                            },
                            format!(
                                "Your router opens ports for archipel by itself{}{}. Each server's page gives the address to share.",
                                i.public_ip.as_deref().map(|ip| format!(" (your public address is {ip})")).unwrap_or_default(),
                                if checked { ", and a check from the internet got through" } else { "" },
                            ),
                        )
                    }
                    ReachState::LocalOnly => (
                        "note note-warning reach-panel",
                        "⚠ Only your home network can join".to_string(),
                        format!(
                            "Your router did not open a port for archipel. To let friends in, turn on UPnP (or NAT-PMP) in your router's settings, or forward ports {} (TCP and UDP) to this PC{this_pc}.",
                            i.ports
                        ),
                    ),
                    ReachState::NoPublicAddress => (
                        "note note-warning reach-panel",
                        "⚠ Friends outside can't join from this connection".to_string(),
                        "Your internet provider shares your public address with other customers, or another router sits in front of yours: no setting on your router can let friends in. Players on your home network can still join. Ask your provider for a public IPv4 address of your own.".to_string(),
                    ),
                };
                view! {
                    <section class=format!("{class} internet-card") role="status" aria-live="polite">
                        <strong>
                            {checking.then(|| view! { <span class="spinner spinner-sm" aria-hidden="true"></span>" " })}
                            {title}
                        </strong>
                        <p>{body}</p>
                        {host}
                        {(i.state == ReachState::Internet).then(|| host_warning(verified))}
                        {(i.state == ReachState::Internet).then(|| unchecked(verified))}
                        {firewall}
                        {detail}
                        <div class="internet-actions">
                            <button class="button button-small" on:click=check disabled=checking>
                                "Check again"
                            </button>
                            <a href="/settings" class="link-button small">"Settings"</a>
                        </div>
                    </section>
                }
            }}
        </Show>
    }
}
