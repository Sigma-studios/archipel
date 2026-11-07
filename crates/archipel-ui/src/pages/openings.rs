//! Openings (desktop app): ports archipel forwards on the router for a game
//! hosted from the game itself (a mod's multiplayer, a game's own "host"
//! button), with no server for archipel to run: switched on while one plays.

use std::time::Duration;

use archipel_core::api::{NewOpening, OpeningView};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::components::{ErrorNote, ReachPanel};
use crate::pages::servers::reach_of;
use crate::util::{polled, toasts};

/// Most ports one opening takes from the pool, as archipel allows.
const MAX_PORTS: u16 = 8;

/// `port 30005`, `ports 30005-30006`.
fn port_list(ports: &[u16]) -> String {
    match (ports.first(), ports.last()) {
        (Some(a), Some(b)) if a != b => format!("ports {a}-{b}"),
        (Some(a), _) => format!("port {a}"),
        _ => String::new(),
    }
}

#[component]
pub fn Openings() -> impl IntoView {
    let openings = polled(Some(Duration::from_secs(3)), || async {
        api::get::<Vec<OpeningView>>("/api/openings").await
    });
    let adding = RwSignal::new(false);
    let on_changed = Callback::new(move |_: ()| openings.reload());
    let empty = move || {
        openings
            .data
            .with(|o| o.as_ref().is_some_and(Vec::is_empty))
    };

    view! {
        <section class="openings" aria-labelledby="openings-title">
            <header class="openings-head">
                <div>
                    <h2 class="section-title" id="openings-title">"Games you host yourself"</h2>
                    <p class="muted small">
                        "For a game you host from the game itself (a mod's multiplayer, a game's own \"Host\" button): archipel opens ports on your router for it while it is switched on, as it does for its servers."
                    </p>
                </div>
                <Show when=move || !adding.get()>
                    <button type="button" class="button" on:click=move |_| adding.set(true)>
                        <span aria-hidden="true">"+"</span>
                        " Open ports for a game"
                    </button>
                </Show>
            </header>
            <ErrorNote error=openings.error/>
            <Show when=move || adding.get()>
                <NewOpeningForm adding on_changed/>
            </Show>
            <Show when=move || openings.loaded() && !empty()>
                <div class="grid-cards">
                    <For
                        each=move || openings.data.get().unwrap_or_default()
                        key=|o| o.id
                        children=move |initial: OpeningView| {
                            let id = initial.id;
                            let opening = Memo::new(move |_| {
                                openings.data.with(|l| {
                                    l.iter().flatten().find(|o| o.id == id).cloned()
                                }).unwrap_or_else(|| initial.clone())
                            });
                            view! { <OpeningCard opening on_changed/> }
                        }
                    />
                </div>
            </Show>
        </section>
    }
}

#[component]
fn NewOpeningForm(adding: RwSignal<bool>, on_changed: Callback<()>) -> impl IntoView {
    let name = RwSignal::new(String::new());
    // From archipel's pool, or the game's own port.
    let own_port = RwSignal::new(false);
    let count = RwSignal::new("1".to_string());
    let port = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let request = if own_port.get_untracked() {
            match port.get_untracked().trim().parse::<u16>() {
                Ok(p) if p > 0 => NewOpening {
                    name: name.get_untracked(),
                    count: None,
                    port: Some(p),
                },
                _ => {
                    error.set(Some("A port is a number from 1 to 65535.".into()));
                    return;
                }
            }
        } else {
            match count.get_untracked().trim().parse::<u16>() {
                Ok(n) if (1..=MAX_PORTS).contains(&n) => NewOpening {
                    name: name.get_untracked(),
                    count: Some(n),
                    port: None,
                },
                _ => {
                    error.set(Some(format!("From 1 to {MAX_PORTS} ports.")));
                    return;
                }
            }
        };
        busy.set(true);
        error.set(None);
        spawn_local(async move {
            match api::post::<_, OpeningView>("/api/openings", &request).await {
                Ok(o) => {
                    toasts().ok(if o.custom {
                        format!(
                            "{}: archipel forwards {} when switched on",
                            o.name,
                            port_list(&o.ports)
                        )
                    } else {
                        format!("{}: set your game to host on port {}", o.name, o.ports[0])
                    });
                    adding.set(false);
                    on_changed.run(());
                }
                Err(e) => error.set(Some(e)),
            }
            busy.try_set(false);
        });
    };

    let segment = move |own: bool, label: &'static str| {
        let active = move || own_port.get() == own;
        view! {
            <button type="button" role="radio"
                class=move || if active() { "segment active" } else { "segment" }
                aria-checked=move || active().to_string()
                on:click=move |_| own_port.set(own)>
                {label}
            </button>
        }
    };

    view! {
        <form class="card opening-form" on:submit=submit>
            <label class="field">
                <span class="field-label">"Name"</span>
                <input type="text" bind:value=name required maxlength="40"
                    placeholder="Dyson Sphere Program with friends"/>
            </label>
            <div class="field">
                <span class="field-label" id="opening-ports-label">"Ports"</span>
                <div class="segmented" role="radiogroup" aria-labelledby="opening-ports-label">
                    {segment(false, "Pick them for me")}
                    {segment(true, "The game's own port")}
                </div>
                {move || if own_port.get() {
                    view! {
                        <label class="field">
                            <span class="field-label">"Port"</span>
                            <input type="number" min="1" max="65535" bind:value=port required
                                placeholder="8469" inputmode="numeric"/>
                            <small>"For a game whose port can't be changed. archipel forwards it, but checks from outside only its own ports: it says what your router answered for this one."</small>
                        </label>
                    }.into_any()
                } else {
                    view! {
                        <label class="field">
                            <span class="field-label">"How many"</span>
                            <input type="number" min="1" max=MAX_PORTS.to_string() bind:value=count required
                                inputmode="numeric"/>
                            <small>"archipel picks free ports from its own, already open in this PC's firewall and checked from outside; then set your game to host on the first one. Most games need one."</small>
                        </label>
                    }.into_any()
                }}
            </div>
            <ErrorNote error=error/>
            <div class="form-actions">
                <button type="button" class="button" on:click=move |_| adding.set(false)>"Cancel"</button>
                <button type="submit" class="button button-primary" disabled=busy>
                    {move || if busy.get() { "Adding…" } else { "Add" }}
                </button>
            </div>
        </form>
    }
}

#[component]
fn OpeningCard(opening: Memo<OpeningView>, on_changed: Callback<()>) -> impl IntoView {
    let busy = RwSignal::new(false);
    let is_open = move || opening.with(|o| o.open);
    let act = move |action: &'static str| {
        let id = opening.with_untracked(|o| o.id);
        busy.set(true);
        spawn_local(async move {
            match api::act_json::<OpeningView>(&format!("/api/openings/{id}/{action}")).await {
                Ok(_) => on_changed.run(()),
                Err(e) => toasts().err(e),
            }
            busy.try_set(false);
        });
    };
    let delete = move |_| {
        let (id, name) = opening.with_untracked(|o| (o.id, o.name.clone()));
        let sure = window()
            .confirm_with_message(&format!("Delete {name}? Its ports go back to archipel."))
            .unwrap_or(false);
        if !sure {
            return;
        }
        spawn_local(async move {
            match api::delete(&format!("/api/openings/{id}")).await {
                Ok(()) => {
                    toasts().ok(format!("Deleted {name}"));
                    on_changed.run(());
                }
                Err(e) => toasts().err(e),
            }
        });
    };
    let hint = move || {
        opening.with(|o| {
            if o.custom {
                format!("Your game's {}, TCP and UDP.", port_list(&o.ports))
            } else if o.ports.len() > 1 {
                format!(
                    "Set your game to host on port {} ({} are its, TCP and UDP).",
                    o.ports[0],
                    port_list(&o.ports)
                )
            } else {
                format!(
                    "Set your game to host on port {} (TCP and UDP).",
                    o.ports[0]
                )
            }
        })
    };

    view! {
        <article class=move || if is_open() { "card opening-card running" } else { "card opening-card" }>
            <div class="opening-title">
                <h3 class="opening-name">{move || opening.with(|o| o.name.clone())}</h3>
                <span class=move || if is_open() { "badge badge-good" } else { "badge badge-neutral" }>
                    <span class="badge-icon" aria-hidden="true">{move || if is_open() { "●" } else { "■" }}</span>
                    {move || if is_open() { "open" } else { "closed" }}
                </span>
            </div>
            <p class="small">{hint}</p>
            {move || if is_open() {
                view! {
                    <ReachPanel reach=Signal::derive(move || opening.with(|o| reach_of(o.reach.as_ref())))/>
                }.into_any()
            } else {
                view! {
                    <p class="small muted">"Closed on the router. Switch it on when you host, and off when you're done; closing archipel closes it too."</p>
                }.into_any()
            }}
            <div class="server-foot">
                {move || if is_open() {
                    view! {
                        <button type="button" class="button" disabled=busy on:click=move |_| act("close")>
                            <span class="button-icon" aria-hidden="true">"■"</span>
                            {move || if busy.get() { "Closing…" } else { "Close ports" }}
                        </button>
                    }.into_any()
                } else {
                    view! {
                        <button type="button" class="button button-go" disabled=busy on:click=move |_| act("open")>
                            <span class="button-icon" aria-hidden="true">"▶"</span>
                            {move || if busy.get() { "Opening…" } else { "Open ports" }}
                        </button>
                    }.into_any()
                }}
                <button type="button" class="link-button danger small" on:click=delete>"Delete"</button>
            </div>
        </article>
    }
}
