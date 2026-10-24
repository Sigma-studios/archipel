//! The desktop app's settings: the hostname friends use, and checking from outside.

use std::time::Duration;

use archipel_core::api::{Internet, ReachState, Settings};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::components::{ErrorNote, Loading};
use crate::util::{desktop, polled, toasts};

#[component]
pub fn SettingsPage() -> impl IntoView {
    if !desktop() {
        return view! { <leptos_router::components::Redirect path="/"/> }.into_any();
    }
    let saved = polled(None, || async {
        api::get::<Settings>("/api/settings").await
    });
    let internet = polled(Some(Duration::from_secs(3)), || async {
        api::get::<Internet>("/api/internet").await
    });
    let host = RwSignal::new(String::new());
    let checking = RwSignal::new(true);
    let url = RwSignal::new(String::new());
    let default_url = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    // The form shows what is saved, once loaded and after each save.
    Effect::new(move |_| {
        if let Some(s) = saved.data.get() {
            host.set(s.public_host.clone());
            checking.set(!s.probe_url.is_empty());
            url.set(if s.probe_url.is_empty() {
                s.default_probe_url.clone()
            } else {
                s.probe_url.clone()
            });
            default_url.set(s.default_probe_url);
        }
    });
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        error.set(None);
        busy.set(true);
        let body = Settings {
            probe_url: if checking.get_untracked() {
                url.get_untracked()
            } else {
                String::new()
            },
            default_probe_url: String::new(),
            public_host: host.get_untracked(),
        };
        spawn_local(async move {
            match api::put::<_, Settings>("/api/settings", &body).await {
                Ok(s) => {
                    saved.data.set(Some(s));
                    internet.reload();
                    toasts().ok("Settings saved");
                }
                Err(e) => error.set(Some(e)),
            }
            busy.set(false);
        });
    };
    view! {
        <div class="page page-narrow">
            <header class="page-head">
                <div>
                    <h1>"Settings"</h1>
                    <p class="muted subtitle">"How friends reach the servers on this PC."</p>
                </div>
            </header>
            <Show when=move || saved.loaded() fallback=Loading>
                <form class="card form" on:submit=submit>
                    <fieldset class="form-section">
                        <legend>"Address for friends"</legend>
                        <label class="field">
                            <span class="field-label">"Hostname"</span>
                            <input type="text" bind:value=host placeholder="yourname.ddns.net"
                                autocapitalize="none" spellcheck="false"/>
                            <span class="field-help">
                                "A dynamic DNS name (No-IP, DuckDNS…) that follows your address. Shown to friends only when it points to this connection."
                            </span>
                        </label>
                        <HostResult saved internet/>
                    </fieldset>
                    <fieldset class="form-section">
                        <legend>"Check from outside"</legend>
                        <div class="switch-field">
                            <label class="switch">
                                <input type="checkbox" role="switch" bind:checked=checking/>
                                <span>"Check that friends get through"</span>
                            </label>
                            <span class="field-help">
                                "Asks an archipel on the internet to try to reach your servers. It only sees your public address and the ports."
                            </span>
                        </div>
                        <label class="field">
                            <span class="field-label">"The archipel to ask"</span>
                            <input type="text" bind:value=url disabled=move || !checking.get()
                                autocapitalize="none" spellcheck="false"/>
                        </label>
                        <Show when=move || checking.get() && url.get().trim_end_matches('/') != default_url.get()>
                            <button type="button" class="link-button small"
                                on:click=move |_| url.set(default_url.get_untracked())>
                                "Reset to default"
                            </button>
                        </Show>
                    </fieldset>
                    <ErrorNote error=error/>
                    <div class="form-actions">
                        <a href="/" class="button">"Back"</a>
                        <button type="submit" class="button button-primary" disabled=busy>
                            {move || if busy.get() { "Saving…" } else { "Save" }}
                        </button>
                    </div>
                </form>
            </Show>
        </div>
    }
    .into_any()
}

/// Where the saved hostname leads, as the last check from outside saw it.
#[component]
fn HostResult(
    saved: crate::util::Polled<Settings>,
    internet: crate::util::Polled<Internet>,
) -> impl IntoView {
    let result = move || {
        let name = saved
            .data
            .with(|s| s.as_ref().map(|s| s.public_host.clone()))
            .filter(|n| !n.is_empty())?;
        let i = internet.data.get()?;
        let (class, text) = match (i.state, i.verified) {
            (ReachState::Checking, _) => ("small muted", "Checking…".to_string()),
            (ReachState::Internet, Some(v)) => match v.host.filter(|h| h.name == name) {
                Some(h) if h.matches => (
                    "small reach-good",
                    format!("✓ {name} points to this connection"),
                ),
                Some(_) => (
                    "small reach-warn",
                    format!(
                        "⚠ {}",
                        v.host_note
                            .unwrap_or_else(|| format!("{name} doesn't lead here"))
                    ),
                ),
                None => (
                    "small muted",
                    format!(
                        "Not checked{}",
                        v.detail.map(|d| format!(": {d}")).unwrap_or_default()
                    ),
                ),
            },
            _ => (
                "small muted",
                "Not checked: friends can't reach this PC from the internet yet".to_string(),
            ),
        };
        Some(view! { <p class=class role="status">{text}</p> })
    };
    view! { {result} }
}
