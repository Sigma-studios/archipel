use archipel_core::api::{ChangePassword, Role};
use leptos::prelude::*;

use crate::api;
use crate::components::{ErrorNote, Loading, QuotaMeters};
use crate::util::{date, run, use_me};

#[component]
pub fn Account() -> impl IntoView {
    let me = use_me();
    view! {
        <div class="page page-narrow">
            <header class="page-head"><h1>"Your account"</h1></header>
            <Show when=move || me.loaded() fallback=Loading>
                {move || me.data.get().map(|u| view! {
                    <section class="card">
                        <div class="cell-user">
                            <span class="avatar" style="width: 44px; height: 44px; font-size: 19px" aria-hidden="true">
                                {u.username.chars().next().map(String::from)}
                            </span>
                            <div>
                                <h2>{u.username.clone()}</h2>
                                <div class="muted small">
                                    {if u.role == Role::Admin { "Admin" } else { "Player" }}
                                    {format!(" · here since {}", date(u.created_at))}
                                </div>
                            </div>
                        </div>
                    </section>
                })}
                <section class="card quota-card">
                    <h2 class="section-title">"Your share of the machine"</h2>
                    <QuotaMeters user=Signal::derive(move || me.data.get().expect("loaded"))/>
                    <p class="muted small">"Need more? Ask the person who runs this archipel."</p>
                </section>
            </Show>
            <Password/>
        </div>
    }
}

#[component]
fn Password() -> impl IntoView {
    let current = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let again = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if password.get_untracked().is_empty() {
            return error.set(Some("Choose a new password.".into()));
        }
        if password.get_untracked() != again.get_untracked() {
            return error.set(Some("The two new passwords are different.".into()));
        }
        error.set(None);
        let body = ChangePassword {
            current: current.get_untracked(),
            new: password.get_untracked(),
        };
        run(
            "Password changed",
            async move { api::put_empty("/api/me/password", &body).await },
            move || {
                current.set(String::new());
                password.set(String::new());
                again.set(String::new());
            },
        );
    };
    view! {
        <section class="card">
            <h2 class="section-title">"Change your password"</h2>
            <form class="form" on:submit=submit>
                <label class="field">
                    <span class="field-label">"Current password"</span>
                    <input type="password" bind:value=current required autocomplete="current-password"/>
                </label>
                <div class="form-row">
                    <label class="field">
                        <span class="field-label">"New password"</span>
                        <input type="password" bind:value=password required autocomplete="new-password"/>
                    </label>
                    <label class="field">
                        <span class="field-label">"Repeat it"</span>
                        <input type="password" bind:value=again required autocomplete="new-password"/>
                    </label>
                </div>
                <ErrorNote error=error/>
                <div class="form-actions">
                    <button type="submit" class="button button-primary">"Change password"</button>
                </div>
            </form>
        </section>
    }
}
