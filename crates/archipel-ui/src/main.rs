//! The archipel web UI: a client-side Leptos app served by `archipel` next to the API.

mod api;
mod components;
mod pages;
mod util;

use std::time::Duration;

use archipel_core::api::{About, LoginRequest, LoginResponse, Mode, UserView};
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::{A, Outlet, ParentRoute, Redirect, Route, Router, Routes};
use leptos_router::hooks::use_navigate;
use leptos_router::path;

use components::BrandMark;
use util::{Me, desktop, is_admin, polled};

fn main() {
    console_error_panic_hook::set_once();
    mount_to_body(App);
}

/// Asks how archipel runs (hosted, or the desktop app) before showing any page:
/// it decides whether there is a login at all.
#[component]
fn App() -> impl IntoView {
    let ready = RwSignal::new(false);
    spawn_local(async move {
        // An archipel without `/api/about` predates the desktop app: hosted.
        let about = api::get::<About>("/api/about").await.ok();
        util::set_mode(about.as_ref().map_or(Mode::Hosted, |a| a.mode));
        util::set_version(about.map(|a| a.version).unwrap_or_default());
        ready.set(true);
    });
    view! {
        <Show when=move || ready.get() fallback=|| view! {
            <div class="login"><components::Loading/></div>
        }>
            <Pages/>
            <footer class="app-version muted small">
                {move || {
                    let v = util::version();
                    (!v.is_empty()).then(|| format!("archipel {v}"))
                }}
            </footer>
        </Show>
        <ToastHost/>
    }
}

#[component]
fn Pages() -> impl IntoView {
    view! {
        <Router>
            <Routes fallback=|| view! { <NotFound/> }>
                <Route path=path!("/login") view=Login/>
                <ParentRoute path=path!("") view=Shell>
                    <Route path=path!("") view=pages::servers::Servers/>
                    <Route path=path!("servers/new") view=pages::new_server::NewServer/>
                    <Route path=path!("servers/:id") view=pages::server::ServerPage/>
                    <Route path=path!("admin") view=pages::admin::Admin/>
                    <Route path=path!("account") view=pages::account::Account/>
                </ParentRoute>
            </Routes>
        </Router>
    }
}

#[component]
fn NotFound() -> impl IntoView {
    view! {
        <div class="login">
            <div class="empty">
                <div class="empty-art" aria-hidden="true">"🧭"</div>
                <h2>"Nothing here"</h2>
                <p>"This page doesn't exist, or it sailed away."</p>
                <a href="/" class="button button-primary">"Back to my servers"</a>
            </div>
        </div>
    }
}

/// Top bar layout for every page but login. Sends visitors without a session to
/// the login page, and shares the signed-in user with the pages.
#[component]
fn Shell() -> impl IntoView {
    let navigate = use_navigate();
    if !desktop() && api::token().is_none() {
        navigate("/login", Default::default());
    }
    let me: Me = polled(Some(Duration::from_secs(10)), || async {
        api::get::<UserView>("/api/me").await
    });
    provide_context(me);
    let username = move || {
        me.data
            .with(|u| u.as_ref().map(|u| u.username.clone()).unwrap_or_default())
    };
    let logout = move |_| {
        spawn_local(async move {
            let _ = api::act("/api/logout").await;
            api::set_token(None);
            let _ = window().location().set_href("/login");
        });
    };
    view! {
        <div class="shell">
            <header class="topbar">
                <nav class="topbar-inner" aria-label="Main">
                    <A href="/" attr:class="brand">
                        <BrandMark/>
                        <span class="brand-name">"archipel"</span>
                    </A>
                    <ul class="nav">
                        <li><A href="/" exact=true>"Servers"</A></li>
                        <Show when=move || is_admin(me)>
                            <li><A href="/admin">"Admin"</A></li>
                        </Show>
                    </ul>
                    <Show when=|| !desktop()>
                    <div class="user-menu">
                        <A href="/account" attr:class="user-chip" attr:title="Your account">
                            <span class="avatar" aria-hidden="true">
                                {move || username().chars().next().map(String::from).unwrap_or_default()}
                            </span>
                            <span class="user-name">{username}</span>
                            <span class="visually-hidden">" (account)"</span>
                        </A>
                        <button type="button" class="nav-logout" on:click=logout>"Log out"</button>
                    </div>
                    </Show>
                </nav>
            </header>
            <main class="content">
                <Outlet/>
            </main>
        </div>
    }
}

#[component]
fn Login() -> impl IntoView {
    if desktop() {
        return view! { <Redirect path="/"/> }.into_any();
    }
    let username = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let navigate = use_navigate();

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        busy.set(true);
        error.set(None);
        let navigate = navigate.clone();
        spawn_local(async move {
            let request = LoginRequest {
                username: username.get_untracked().trim().to_string(),
                password: password.get_untracked(),
            };
            match api::post::<_, LoginResponse>("/api/login", &request).await {
                Ok(LoginResponse { token, .. }) => {
                    api::set_token(Some(&token));
                    navigate("/", Default::default());
                }
                Err(e) => error.set(Some(e)),
            }
            busy.set(false);
        });
    };

    view! {
        <main class="login">
            <form class="card login-card" on:submit=submit>
                <div class="brand login-brand">
                    <BrandMark/>
                    "archipel"
                </div>
                <p class="login-tag">"Your game servers, one click away."</p>
                <label class="field">
                    <span class="field-label">"Username"</span>
                    <input type="text" autocomplete="username" bind:value=username required autofocus
                        autocapitalize="none" spellcheck="false"/>
                </label>
                <label class="field">
                    <span class="field-label">"Password"</span>
                    <input type="password" autocomplete="current-password" bind:value=password required/>
                </label>
                <components::ErrorNote error=error/>
                <button class="button button-primary button-big" type="submit" disabled=busy>
                    {move || if busy.get() { "Signing in…" } else { "Sign in" }}
                </button>
            </form>
        </main>
    }
    .into_any()
}

#[component]
fn ToastHost() -> impl IntoView {
    let toasts = util::toasts();
    view! {
        <div class="toasts" aria-live="polite">
            <For each=move || toasts.0.get() key=|t| t.id let:toast>
                <div class=if toast.error { "toast toast-error" } else { "toast" }
                    role=if toast.error { "alert" } else { "status" }>
                    <span aria-hidden="true">{if toast.error { "⚠" } else { "✓" }}</span>
                    <span class="toast-text">{toast.message.clone()}</span>
                    <button type="button" class="icon-button" aria-label="Dismiss"
                        on:click=move |_| toasts.dismiss(toast.id)>"✕"</button>
                </div>
            </For>
        </div>
    }
}
