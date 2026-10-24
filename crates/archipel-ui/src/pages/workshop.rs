//! A server's Workshop tab (Garry's Mod): what its gamemode brings from the Steam
//! Workshop, and the items and collections its owner adds.

use archipel_core::api::{
    AddWorkshopItem, ServerState, ServerView, WorkshopItem, WorkshopKind, WorkshopView,
};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::components::{ErrorNote, Loading};
use crate::util::{bytes, plural, polled, run, toasts};

#[component]
pub fn WorkshopTab(
    id: Memo<u64>,
    server: Memo<Option<ServerView>>,
    /// Opens another tab of the server page.
    on_tab: Callback<&'static str>,
) -> impl IntoView {
    let view = polled(None, move || {
        let id = id.get();
        // The gamemode decides the preset shown.
        server.with(|s| s.as_ref().map(|s| s.settings.clone()));
        async move { api::get::<WorkshopView>(&format!("/api/servers/{id}/workshop")).await }
    });
    let link = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let changed = RwSignal::new(false);
    let running = move || {
        server.with(|s| {
            s.as_ref()
                .is_some_and(|s| matches!(s.state, ServerState::Running | ServerState::Starting))
        })
    };
    let add = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let text = link.get_untracked().trim().to_string();
        if text.is_empty() {
            return;
        }
        let id = id.get_untracked();
        busy.set(true);
        spawn_local(async move {
            let body = AddWorkshopItem { link: text };
            match api::post::<_, WorkshopView>(&format!("/api/servers/{id}/workshop"), &body).await
            {
                Ok(v) => {
                    if let Some(item) = v.items.last() {
                        toasts().ok(format!("{} added", item.title));
                    }
                    view.data.try_set(Some(v));
                    link.try_set(String::new());
                    changed.try_set(true);
                }
                Err(e) => toasts().err(e),
            }
            busy.try_set(false);
        });
    };
    let remove = Callback::new(move |item: WorkshopItem| {
        if !window()
            .confirm_with_message(&format!("Remove {} from this server?", item.title))
            .unwrap_or(false)
        {
            return;
        }
        let id = id.get_untracked();
        spawn_local(async move {
            match api::delete_json::<WorkshopView>(&format!(
                "/api/servers/{id}/workshop/{}",
                item.id
            ))
            .await
            {
                Ok(v) => {
                    toasts().ok(format!("{} removed", item.title));
                    view.data.try_set(Some(v));
                    changed.try_set(true);
                }
                Err(e) => toasts().err(e),
            }
        });
    });
    let restart = move |_| {
        let id = id.get_untracked();
        run(
            "Restarting…",
            async move { api::act(&format!("/api/servers/{id}/restart")).await },
            move || changed.set(false),
        );
    };
    let items = move || {
        view.data
            .with(|v| v.as_ref().map(|v| v.items.clone()).unwrap_or_default())
    };

    view! {
        <ErrorNote error=view.error/>
        <Show when=move || changed.get() && running()>
            <div class="note note-info" role="status">
                "The server downloads them when it starts. "
                <button type="button" class="button button-small" on:click=restart>"↻ Restart now"</button>
            </div>
        </Show>
        {move || view.data.with(|v| v.as_ref().and_then(|v| v.steam_error.clone())).map(|e| view! {
            <div class="note note-warning">{format!("Steam did not answer, so items show their numbers only: {e}")}</div>
        })}
        <Show when=move || view.loaded() fallback=Loading>
            {move || view.data.with(|v| v.as_ref().map(|v| v.preset.clone())).map(|preset| match preset {
                Some(p) => {
                    let total: u32 = p.maps.iter().map(|m| m.mb).sum();
                    view! {
                        <section class="card addons-card" aria-label="From the gamemode">
                            <div class="card-head">
                                <div>
                                    <h2 class="section-title">{p.gamemode.clone()}</h2>
                                    <p class="muted small addons-help">
                                        {format!("What this gamemode needs comes along: {} and {} ({} MB), picked to work without Counter-Strike: Source. The server plays them in turn; players download each as it comes up. ",
                                            plural(p.items.len() as u64, "item", "items"),
                                            plural(p.maps.len() as u64, "map", "maps"),
                                            total)}
                                        <button type="button" class="link-button" on:click=move |_| on_tab.run("settings")>"Change the gamemode or first map in Settings"</button>
                                    </p>
                                </div>
                            </div>
                            <ul class="addon-list">
                                {p.items.into_iter().map(|item| view! { <ItemRow item/> }).collect_view()}
                            </ul>
                            <div class="preset-maps">
                                {p.maps.into_iter().map(|m| view! {
                                    <a class="preset-map" target="_blank" rel="noopener noreferrer"
                                        href=format!("https://steamcommunity.com/sharedfiles/filedetails/?id={}", m.id)>
                                        <span class="addon-name">{m.label}</span>
                                        <span class="muted small">{format!("{} · {} MB", m.map, m.mb)}</span>
                                    </a>
                                }).collect_view()}
                            </div>
                        </section>
                    }.into_any()
                }
                None => view! {
                    <div class="note">
                        "archipel has nothing to add for this gamemode. Sandbox needs nothing; for another one, add it and its maps below. "
                        <button type="button" class="link-button" on:click=move |_| on_tab.run("settings")>"Gamemodes are in Settings"</button>
                    </div>
                }.into_any(),
            })}
            <section class="card addons-card" aria-label="Your Workshop items">
                <div class="card-head">
                    <div>
                        <h2 class="section-title">"Your Workshop items"</h2>
                        <p class="muted small addons-help">
                            "Addons, maps, gamemodes or whole collections from the "
                            <a href="https://steamcommunity.com/app/4000/workshop/" target="_blank" rel="noopener noreferrer">"Steam Workshop ↗"</a>
                            ": copy the address of an item's page, paste it here. The server downloads them at its next start. Maps and gamemodes found in them then show up in Settings; players download the map being played, the gamemode, and the models and sounds of the rest."
                        </p>
                    </div>
                </div>
                <form class="toolbar" on:submit=add>
                    <label for="workshop-link" class="visually-hidden">"Workshop link"</label>
                    <input type="text" id="workshop-link" class="grow" bind:value=link autocomplete="off"
                        placeholder="https://steamcommunity.com/sharedfiles/filedetails/?id=…"/>
                    <button type="submit" class="button button-primary" disabled=move || busy.get()>
                        {move || if busy.get() { "Adding…" } else { "Add" }}
                    </button>
                </form>
                <Show when=move || !items().is_empty() fallback=|| view! {
                    <p class="muted">"None yet."</p>
                }>
                    <ul class="addon-list">
                        <For each=items key=|i| i.id.clone() let:item>
                            <ItemRow item on_remove=remove/>
                        </For>
                    </ul>
                </Show>
            </section>
        </Show>
    }
}

#[component]
fn ItemRow(
    item: WorkshopItem,
    #[prop(optional)] on_remove: Option<Callback<WorkshopItem>>,
) -> impl IntoView {
    let kind = match item.kind {
        WorkshopKind::Collection => format!(
            "Collection of {}",
            plural(item.children.len() as u64, "item", "items")
        ),
        WorkshopKind::Map => "Map".into(),
        WorkshopKind::Gamemode => "Gamemode".into(),
        WorkshopKind::Addon => "Addon".into(),
    };
    let size = (item.bytes > 0).then(|| format!(" · {}", bytes(item.bytes)));
    let found = {
        let mut parts = Vec::new();
        if !item.gamemodes.is_empty() {
            parts.push(format!("gamemodes: {}", item.gamemodes.join(", ")));
        }
        if !item.maps.is_empty() {
            let shown: Vec<String> = item.maps.iter().take(12).cloned().collect();
            let more = item.maps.len().saturating_sub(shown.len());
            let mut maps = format!(
                "{}: {}",
                plural(item.maps.len() as u64, "map", "maps"),
                shown.join(", ")
            );
            if more > 0 {
                maps.push_str(&format!(" and {more} more"));
            }
            parts.push(maps);
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    };
    let problem = item.problem.clone();
    let shown = item.clone();
    view! {
        <li class="addon-item">
            {match shown.preview_url.clone() {
                Some(src) => view! { <img class="catalogue-icon" src=src alt="" loading="lazy"/> }.into_any(),
                None => view! { <div class="catalogue-icon" aria-hidden="true">"🧰"</div> }.into_any(),
            }}
            <div class="grow">
                <a class="addon-name" href=shown.url.clone() target="_blank" rel="noopener noreferrer">{shown.title.clone()}</a>
                <div class="muted small">{kind}{size}</div>
                {found.map(|f| view! { <div class="addon-maps">{f}</div> })}
                {problem.map(|p| view! { <div class="addon-problem">{format!("⚠ {p}")}</div> })}
            </div>
            {on_remove.map(|cb| view! {
                <button type="button" class="button button-small" on:click=move |_| cb.run(item.clone())>"Remove"</button>
            })}
        </li>
    }
}
