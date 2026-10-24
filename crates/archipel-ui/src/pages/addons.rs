//! A server's addons tab ("Plugins", "Maps"): the files its owner added, uploaded
//! here or installed from the game's catalogue (Modrinth, for Paper plugins).

use archipel_core::api::{
    AddonFile, AddonResult, AddonsView, InstallAddon, InstalledAddons, ServerState, ServerView,
};
use archipel_core::game::AddonsDef;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::components::{ErrorNote, Loading};
use crate::util::{bytes, date, plural, polled, run, toasts};

/// What the server takes in one upload (archipel's own limit).
const MAX_UPLOAD: f64 = 512.0 * 1024.0 * 1024.0;

#[component]
pub fn AddonsTab(
    id: Memo<u64>,
    def: AddonsDef,
    server: Memo<Option<ServerView>>,
    /// Opens another tab of the server page (Settings, when a setting is missing).
    on_tab: Callback<&'static str>,
) -> impl IntoView {
    let addons = polled(None, move || {
        let id = id.get();
        async move { api::get::<AddonsView>(&format!("/api/servers/{id}/addons")).await }
    });
    // Something changed since the server started: it needs a restart.
    let changed = RwSignal::new(false);
    let uploading = RwSignal::new(None::<String>);
    let dragging = RwSignal::new(false);
    let running = move || {
        server.with(|s| {
            s.as_ref()
                .is_some_and(|s| matches!(s.state, ServerState::Running | ServerState::Starting))
        })
    };
    let label = def.label.clone();
    let lower = label.to_lowercase();
    let accept = def
        .extensions
        .iter()
        .map(|e| format!(".{e}"))
        .collect::<Vec<_>>()
        .join(",");
    let kinds = def
        .extensions
        .iter()
        .map(|e| format!(".{e}"))
        .collect::<Vec<_>>()
        .join(" or ");

    // Uploads the files one after the other.
    let upload = move |files: Vec<web_sys::File>| {
        if files.is_empty() || uploading.get_untracked().is_some() {
            return;
        }
        let id = id.get_untracked();
        spawn_local(async move {
            let mut done = 0;
            for file in files {
                let name = file.name();
                if file.size() > MAX_UPLOAD {
                    toasts().err(format!("{name} is larger than 512 MB."));
                    continue;
                }
                uploading.try_set(Some(name.clone()));
                let path = format!(
                    "/api/servers/{id}/addons/files/{}",
                    String::from(js_sys::encode_uri_component(&name))
                );
                match api::upload(&path, file).await {
                    Ok(()) => done += 1,
                    Err(e) => toasts().err(format!("{name}: {e}")),
                }
            }
            uploading.try_set(None);
            if done > 0 {
                toasts().ok(format!("{} added", plural(done, "file", "files")));
                changed.try_set(true);
                addons.reload();
            }
        });
    };
    let on_pick = move |ev: leptos::ev::Event| {
        let input: web_sys::HtmlInputElement = event_target(&ev);
        let files = input
            .files()
            .map(|list| files_of(&list))
            .unwrap_or_default();
        // The same file may be picked again.
        input.set_value("");
        upload(files);
    };
    let on_drop = move |ev: leptos::ev::DragEvent| {
        ev.prevent_default();
        dragging.set(false);
        let files = ev
            .data_transfer()
            .and_then(|d| d.files())
            .map(|list| files_of(&list))
            .unwrap_or_default();
        upload(files);
    };
    let remove = move |name: String| {
        if !window()
            .confirm_with_message(&format!("Remove {name} from this server?"))
            .unwrap_or(false)
        {
            return;
        }
        let id = id.get_untracked();
        let path = format!(
            "/api/servers/{id}/addons/files/{}",
            String::from(js_sys::encode_uri_component(&name))
        );
        run(
            format!("{name} removed"),
            async move { api::delete(&path).await },
            move || {
                changed.set(true);
                addons.reload();
            },
        );
    };
    let restart = move |_| {
        let id = id.get_untracked();
        run(
            "Restarting…",
            async move { api::act(&format!("/api/servers/{id}/restart")).await },
            move || changed.set(false),
        );
    };
    let unmet = move || {
        addons
            .data
            .with(|a| a.as_ref().map(|a| a.unmet.clone()).unwrap_or_default())
    };
    let files = move || {
        addons
            .data
            .with(|a| a.as_ref().map(|a| a.files.clone()).unwrap_or_default())
    };
    let searchable = def.search.is_some();
    let help = def.help.clone();
    let empty_title = format!("No {lower} yet");
    let label_title = label.clone();
    let unmet_intro = format!("{label} need these settings: ");

    view! {
        <ErrorNote error=addons.error/>
        <Show when=move || !unmet().is_empty()>
            <div class="note note-warning" role="status">
                {unmet_intro.clone()}
                <strong>{move || unmet().join(", ")}</strong>
                ". "
                <button type="button" class="link-button" on:click=move |_| on_tab.run("settings")>"Change them in Settings"</button>
            </div>
        </Show>
        <Show when=move || changed.get() && running()>
            <div class="note note-info" role="status">
                "The server loads them when it starts. "
                <button type="button" class="button button-small" on:click=restart>"↻ Restart now"</button>
            </div>
        </Show>
        <section class=move || if dragging.get() { "card addons-card dragging" } else { "card addons-card" }
            aria-label=label.clone()
            on:dragover=move |ev: leptos::ev::DragEvent| {
                ev.prevent_default();
                dragging.set(true);
            }
            on:dragleave=move |_| dragging.set(false)
            on:drop=on_drop>
            <div class="card-head">
                <div>
                    <h2 class="section-title">{label_title}</h2>
                    <p class="muted small addons-help">{help}</p>
                </div>
                <label class=move || if uploading.get().is_some() { "button button-primary disabled" } else { "button button-primary" }>
                    {move || match uploading.get() {
                        Some(name) => format!("Uploading {name}…"),
                        None => "Upload…".to_string(),
                    }}
                    <input type="file" class="visually-hidden" multiple accept=accept
                        disabled=move || uploading.get().is_some() on:change=on_pick/>
                </label>
            </div>
            <Show when=move || addons.loaded() fallback=Loading>
                <Show when=move || !files().is_empty() fallback={
                    let (title, kinds) = (empty_title.clone(), kinds.clone());
                    move || view! {
                        <div class="empty addons-empty">
                            <h2>{title.clone()}</h2>
                            <p>{format!("Drop {kinds} files here, or use Upload.")}</p>
                        </div>
                    }
                }>
                    <ul class="addon-list">
                        <For each=files key=|f| (f.name.clone(), f.modified, f.bytes) let:file>
                            <AddonRow file on_remove=Callback::new(remove)/>
                        </For>
                    </ul>
                    <p class="muted small">{move || format!("{}. Drop more here to add them.", plural(files().len() as u64, "file", "files"))}</p>
                </Show>
            </Show>
        </section>
        {searchable.then(|| view! {
            <Show when=move || addons.loaded() && unmet().is_empty()>
                <CatalogueSearch id
                    version=Signal::derive(move || addons.data.with(|a| a.as_ref().and_then(|a| a.game_version.clone())))
                    on_installed=Callback::new(move |_| {
                        changed.set(true);
                        addons.reload();
                    })/>
            </Show>
        })}
    }
}

fn files_of(list: &web_sys::FileList) -> Vec<web_sys::File> {
    (0..list.length()).filter_map(|i| list.get(i)).collect()
}

#[component]
fn AddonRow(file: AddonFile, on_remove: Callback<String>) -> impl IntoView {
    let name = file.name.clone();
    let maps = (!file.maps.is_empty()).then(|| {
        view! {
            <div class="addon-maps">
                {plural(file.maps.len() as u64, "map", "maps")} ": "
                {file.maps.iter().map(|m| view! { <code>{m.clone()}</code> }).collect_view()}
            </div>
        }
    });
    view! {
        <li class="addon-item">
            <div class="grow">
                <div class="addon-name">{file.name.clone()}</div>
                <div class="muted small">{format!("{} · added {}", bytes(file.bytes), date(file.modified))}</div>
                {maps}
            </div>
            <button type="button" class="button button-small"
                on:click=move |_| on_remove.run(name.clone())>"Remove"</button>
        </li>
    }
}

/// Plugins from Modrinth, for the server's Minecraft version.
#[component]
fn CatalogueSearch(
    id: Memo<u64>,
    version: Signal<Option<String>>,
    on_installed: Callback<()>,
) -> impl IntoView {
    let typed = RwSignal::new(String::new());
    let query = RwSignal::new(String::new());
    let results = polled(None, move || {
        let (id, q) = (id.get(), query.get());
        let q = String::from(js_sys::encode_uri_component(&q));
        async move {
            api::get::<Vec<AddonResult>>(&format!("/api/servers/{id}/addons/search?q={q}")).await
        }
    });
    let installing = RwSignal::new(None::<String>);
    let install = move |result: AddonResult| {
        let id = id.get_untracked();
        installing.set(Some(result.id.clone()));
        spawn_local(async move {
            let body = InstallAddon { id: result.id };
            match api::post::<_, InstalledAddons>(
                &format!("/api/servers/{id}/addons/install"),
                &body,
            )
            .await
            {
                Ok(done) => {
                    let mut message = format!("{} installed", result.title);
                    if done.files.len() > 1 {
                        message.push_str(&format!(", with {}", done.files[1..].join(", ")));
                    }
                    toasts().ok(message);
                    on_installed.run(());
                }
                Err(e) => toasts().err(e),
            }
            installing.try_set(None);
        });
    };
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        query.set(typed.get_untracked().trim().to_string());
    };

    view! {
        <section class="card catalogue" aria-label="Find plugins">
            <div class="card-head">
                <div>
                    <h2 class="section-title">"Find plugins"</h2>
                    <p class="muted small">
                        {move || match version.get() {
                            Some(v) => format!("Paper plugins for Minecraft {v}, from "),
                            None => "Paper plugins, from ".to_string(),
                        }}
                        <a href="https://modrinth.com/plugins" target="_blank" rel="noopener noreferrer">"Modrinth ↗"</a>
                        ". Plugins they require come along."
                    </p>
                </div>
            </div>
            <form class="toolbar" on:submit=submit role="search">
                <label for="addon-search" class="visually-hidden">"Search plugins"</label>
                <input type="search" id="addon-search" class="grow" bind:value=typed autocomplete="off"
                    placeholder="Search: EssentialsX, LuckPerms, WorldEdit…"/>
                <button type="submit" class="button">"Search"</button>
            </form>
            <ErrorNote error=results.error/>
            <Show when=move || results.loaded() fallback=Loading>
                <Show when=move || results.data.with(|r| r.as_ref().is_some_and(|r| !r.is_empty()))
                    fallback=|| view! { <p class="muted">"Nothing found for this version."</p> }>
                    <p class="muted small">{move || if query.with(String::is_empty) { "The most downloaded:" } else { "Found:" }}</p>
                    <ul class="catalogue-list">
                        <For each=move || results.data.get().unwrap_or_default() key=|r| r.id.clone() let:result>
                            {
                                let busy = {
                                    let id = result.id.clone();
                                    move || installing.with(|i| i.as_deref() == Some(id.as_str()))
                                };
                                let shown = result.clone();
                                view! {
                                    <li class="catalogue-item">
                                        {match shown.icon_url.clone() {
                                            Some(src) => view! { <img class="catalogue-icon" src=src alt="" loading="lazy"/> }.into_any(),
                                            None => view! { <div class="catalogue-icon" aria-hidden="true">"🧩"</div> }.into_any(),
                                        }}
                                        <div class="grow">
                                            <div>
                                                <a class="addon-name" href=shown.url.clone() target="_blank" rel="noopener noreferrer">{shown.title.clone()}</a>
                                                <span class="muted small">{format!(" by {} · {} downloads", shown.author, count(shown.downloads))}</span>
                                            </div>
                                            <div class="muted small">{shown.description.clone()}</div>
                                        </div>
                                        <button type="button" class="button button-small"
                                            disabled=move || installing.get().is_some()
                                            on:click=move |_| install(result.clone())>
                                            {move || if busy() { "Installing…" } else { "Install" }}
                                        </button>
                                    </li>
                                }
                            }
                        </For>
                    </ul>
                </Show>
            </Show>
        </section>
    }
}

/// 1234567 as "1.2 M".
fn count(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.0} k", n as f64 / 1e3),
        _ => format!("{:.1} M", n as f64 / 1e6),
    }
}
