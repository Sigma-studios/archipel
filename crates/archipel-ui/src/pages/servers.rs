use std::time::Duration;

use archipel_core::api::{Reach, ReachState, ServerState, ServerView};
use archipel_core::game::GameDef;
use leptos::prelude::*;

use crate::api;
use crate::components::{
    ConnectAddress, ErrorNote, GameBadge, InternetCard, Loading, Meter, PowerButton, QuotaMeters,
    ReachSummary, StateBadge,
};
use crate::util::{Polled, bytes, desktop, is_admin, mb, percent, polled, use_me};

/// The badge of a game, from the definitions.
pub fn badge_of(games: Polled<Vec<GameDef>>, game: &str) -> String {
    games.data.with(|g| {
        g.iter()
            .flatten()
            .find(|g| g.id == game)
            .map(|g| g.badge.clone())
            .unwrap_or_default()
    })
}

/// A server's reachability, or "checking" while it has none.
pub fn reach_of(reach: Option<&Reach>) -> Reach {
    reach.cloned().unwrap_or(Reach {
        state: ReachState::Checking,
        local: None,
        public: None,
        forward: vec![],
        detail: None,
        firewall: None,
        verified: None,
    })
}

/// Card border class for a state.
pub fn state_class(state: ServerState) -> &'static str {
    match state {
        ServerState::Running => "running",
        ServerState::Crashed => "crashed",
        ServerState::Installing | ServerState::Starting | ServerState::Stopping => "busy",
        ServerState::Stopped => "",
    }
}

#[component]
pub fn Servers() -> impl IntoView {
    let me = use_me();
    let servers = polled(Some(Duration::from_secs(3)), || async {
        api::get::<Vec<ServerView>>("/api/servers").await
    });
    let games = polled(None, || async {
        api::get::<Vec<GameDef>>("/api/games").await
    });
    let admin = move || is_admin(me);
    let username = move || {
        me.data
            .with(|u| u.as_ref().map(|u| u.username.clone()).unwrap_or_default())
    };
    let empty = move || servers.data.with(|s| s.as_ref().is_some_and(Vec::is_empty));
    let on_changed = Callback::new(move |_: ServerView| {
        servers.reload();
        me.reload();
    });

    view! {
        <div class="page">
            <header class="page-head">
                <div>
                    <h1>{move || if admin() { "Servers" } else { "Your servers" }}</h1>
                    <p class="muted subtitle">
                        {move || if desktop() {
                            "Game servers running on this PC.".to_string()
                        } else if admin() {
                            "Every server on this machine, whoever owns it.".to_string()
                        } else {
                            format!("Welcome back, {}!", username())
                        }}
                    </p>
                </div>
                <a href="/servers/new" class="button button-primary button-big">
                    <span aria-hidden="true">"+"</span>
                    "New server"
                </a>
            </header>

            <Show when=desktop>
                <InternetCard/>
            </Show>

            <Show when=move || me.loaded()>
                <section class="card quota-card" aria-labelledby="quota-title">
                    <h2 class="section-title" id="quota-title">
                        {if desktop() { "This PC" } else { "Your share of the machine" }}
                    </h2>
                    <QuotaMeters user=Signal::derive(move || me.data.get().expect("loaded"))/>
                </section>
            </Show>

            <ErrorNote error=servers.error/>
            <Show when=move || servers.loaded() fallback=Loading>
                <Show when=move || !empty() fallback=|| view! {
                    <div class="empty">
                        <div class="empty-art" aria-hidden="true">"🏝️"</div>
                        <h2>"No servers yet"</h2>
                        <p>"Pick a game, give your server a name, and it is ready for your friends in a minute or two."</p>
                        <a href="/servers/new" class="button button-primary button-big">"Create your first server"</a>
                    </div>
                }>
                    <div class="grid-cards">
                        <For
                            each=move || servers.data.get().unwrap_or_default()
                            key=|s| s.id
                            children=move |initial: ServerView| {
                                let id = initial.id;
                                // Follows the list, keeping the card in place as it refreshes.
                                let server = Memo::new(move |_| {
                                    servers.data.with(|l| {
                                        l.iter().flatten().find(|s| s.id == id).cloned()
                                    }).unwrap_or_else(|| initial.clone())
                                });
                                view! {
                                    <ServerCard server games on_changed
                                        show_owner=Signal::derive(move || {
                                            admin() && server.with(|s| s.owner != username())
                                        })/>
                                }
                            }
                        />
                        <a href="/servers/new" class="new-card">
                            <span class="new-card-plus" aria-hidden="true">"+"</span>
                            "New server"
                        </a>
                    </div>
                </Show>
            </Show>
        </div>
    }
}

#[component]
fn ServerCard(
    server: Memo<ServerView>,
    games: Polled<Vec<GameDef>>,
    on_changed: Callback<ServerView>,
    #[prop(into)] show_owner: Signal<bool>,
) -> impl IntoView {
    let state = Signal::derive(move || server.with(|s| s.state));
    let running = move || state.get() == ServerState::Running;
    let usage = move || server.with(|s| s.usage.clone());
    let href = move || format!("/servers/{}", server.with(|s| s.id));
    // Desktop servers say who can join; hosted ones show their address.
    let has_reach = Memo::new(move |_| server.with(|s| s.reach.is_some()));
    view! {
        <article class=move || format!("card server-card {}", state_class(state.get()))>
            <div class="server-head">
                <GameBadge badge=Signal::derive(move || badge_of(games, &server.with(|s| s.game.clone())))
                    game=Signal::derive(move || server.with(|s| s.game.clone()))/>
                <div class="server-title">
                    <a href=href class="server-name">{move || server.with(|s| s.name.clone())}</a>
                    <div class="server-game">
                        {move || server.with(|s| s.game_name.clone())}
                        <Show when=move || show_owner.get()>
                            " · " <span title="Owner">"👤 " {move || server.with(|s| s.owner.clone())}</span>
                        </Show>
                    </div>
                </div>
                <StateBadge state/>
            </div>
            {move || if has_reach.get() {
                view! { <ReachSummary reach=Signal::derive(move || server.with(|s| reach_of(s.reach.as_ref())))/> }.into_any()
            } else {
                view! { <ConnectAddress template=Signal::derive(move || server.with(|s| s.connect.clone()))/> }.into_any()
            }}
            <div class="server-stats">
                <span>"Memory " <strong>{move || mb(server.with(|s| s.memory_mb))}</strong></span>
                {move || usage().filter(|_| running()).map(|u| view! {
                    <span>"CPU " <strong>{format!("{:.0} %", u.cpu_percent)}</strong></span>
                })}
                {move || server.with(|s| s.disk_bytes).map(|d| view! {
                    <span>"Disk " <strong>{bytes(d)}</strong></span>
                })}
            </div>
            {move || usage().filter(|_| running()).map(|u| {
                let limit = server.with(|s| s.memory_mb) * 1024 * 1024;
                view! {
                    <Meter label="Memory in use" percent=percent(u.memory_bytes, limit)
                        detail=format!("{} of {}", bytes(u.memory_bytes), bytes(limit))/>
                }
            })}
            {move || server.with(|s| s.message.clone())
                .filter(|_| matches!(state.get(), ServerState::Crashed | ServerState::Installing))
                .map(|m| view! {
                    <p class=move || if state.get() == ServerState::Crashed { "server-message" } else { "server-message muted" }>{m}</p>
                })}
            <div class="server-foot">
                <PowerButton server on_done=on_changed/>
                <span class="server-open" aria-hidden="true">"Console & settings →"</span>
            </div>
        </article>
    }
}
