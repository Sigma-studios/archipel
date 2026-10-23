use std::time::Duration;

use archipel_core::api::{CreateUser, Overview, Quota, Role, ServerView, UpdateUser, UserView};
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::NavigateOptions;
use leptos_router::hooks::{use_navigate, use_query_map};

use crate::api;
use crate::components::{ErrorNote, Loading, Meter, Modal};
use crate::util::{bytes, cores, date, is_admin, mb, percent, plural, polled, run, toasts, use_me};

const TABS: &[(&str, &str)] = &[("users", "Users"), ("machine", "Machine")];

#[component]
pub fn Admin() -> impl IntoView {
    let me = use_me();
    let query = use_query_map();
    let navigate = use_navigate();
    let tab = Memo::new(move |_| query.read().get("tab").unwrap_or_else(|| "users".into()));
    let navigate = StoredValue::new_local(navigate);
    let set_tab = move |t: &str| {
        navigate.with_value(|navigate| {
            navigate(
                &format!("/admin?tab={t}"),
                NavigateOptions {
                    replace: true,
                    scroll: false,
                    ..Default::default()
                },
            )
        })
    };
    view! {
        <div class="page">
            <header class="page-head">
                <div>
                    <h1>"Admin"</h1>
                    <p class="muted subtitle">"Who plays here, and how much of the machine each one gets."</p>
                </div>
            </header>
            <Show when=move || me.loaded() fallback=Loading>
                <Show when=move || is_admin(me) fallback=|| view! {
                    <div class="empty">
                        <div class="empty-art" aria-hidden="true">"🔒"</div>
                        <h2>"Admins only"</h2>
                        <p>"This page is for the people who run this machine."</p>
                    </div>
                }>
                    <div class="tabs" role="tablist" aria-label="Admin">
                        {TABS.iter().map(|(key, label)| {
                            let key = *key;
                            view! {
                                <button type="button" role="tab" class=move || if tab.get() == key { "tab active" } else { "tab" }
                                    aria-selected=move || (tab.get() == key).to_string()
                                    on:click=move |_| set_tab(key)>
                                    {*label}
                                </button>
                            }
                        }).collect_view()}
                    </div>
                    <div role="tabpanel">
                        {move || match tab.get().as_str() {
                            "machine" => view! { <MachineTab/> }.into_any(),
                            _ => view! { <UsersTab/> }.into_any(),
                        }}
                    </div>
                </Show>
            </Show>
        </div>
    }
}

// ---------------------------------------------------------------- users

/// GB and cores as people type them, with decimals.
fn decimal(value: f64) -> String {
    let text = format!("{value:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// A quota as typed in a form: memory and disk in GB, CPU in cores.
#[derive(Clone, Copy)]
struct QuotaForm {
    memory_gb: RwSignal<String>,
    cpu_cores: RwSignal<String>,
    servers: RwSignal<String>,
    disk_gb: RwSignal<String>,
}

impl QuotaForm {
    fn new() -> Self {
        let form = QuotaForm {
            memory_gb: RwSignal::new(String::new()),
            cpu_cores: RwSignal::new(String::new()),
            servers: RwSignal::new(String::new()),
            disk_gb: RwSignal::new(String::new()),
        };
        form.fill(&Quota::default());
        form
    }

    fn fill(&self, q: &Quota) {
        self.memory_gb.set(decimal(q.memory_mb as f64 / 1024.0));
        self.cpu_cores.set(decimal(q.cpu_percent as f64 / 100.0));
        self.servers.set(q.max_servers.to_string());
        self.disk_gb.set(decimal(q.disk_mb as f64 / 1024.0));
    }

    fn quota(&self) -> Result<Quota, String> {
        let number = |s: RwSignal<String>, what: &str| {
            s.get_untracked()
                .trim()
                .replace(',', ".")
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v >= 0.0)
                .ok_or_else(|| format!("{what} must be a number, 0 or more."))
        };
        Ok(Quota {
            memory_mb: (number(self.memory_gb, "Memory")? * 1024.0).round() as u64,
            cpu_percent: (number(self.cpu_cores, "CPU")? * 100.0).round() as u32,
            max_servers: number(self.servers, "Servers")?.round() as u32,
            disk_mb: (number(self.disk_gb, "Disk")? * 1024.0).round() as u64,
        })
    }
}

#[component]
fn QuotaFields(form: QuotaForm) -> impl IntoView {
    let field = move |label: &'static str,
                      unit: &'static str,
                      step: &'static str,
                      value: RwSignal<String>,
                      help: &'static str| {
        view! {
            <label class="field">
                <span class="field-label">{label}</span>
                <span class="input-suffix">
                    <input type="number" min="0" step=step inputmode="decimal" bind:value=value required/>
                    <span>{unit}</span>
                </span>
                <small>{help}</small>
            </label>
        }
    };
    view! {
        <fieldset class="form-section">
            <legend>"Quota"</legend>
            <div class="form-grid">
                {field("Memory", "GB", "0.5", form.memory_gb, "For all their running servers together.")}
                {field("CPU", "cores", "0.5", form.cpu_cores, "Shared by their running servers.")}
                {field("Servers", "servers", "1", form.servers, "Running or not.")}
                {field("Disk", "GB", "1", form.disk_gb, "Worlds and game files.")}
            </div>
        </fieldset>
    }
}

#[component]
fn RoleField(role: RwSignal<Role>) -> impl IntoView {
    let option = move |value: Role, label: &'static str| {
        view! {
            <button type="button" role="radio"
                class=move || if role.get() == value { "segment active" } else { "segment" }
                aria-checked=move || (role.get() == value).to_string()
                on:click=move |_| role.set(value)>{label}</button>
        }
    };
    view! {
        <div class="field">
            <span class="field-label" id="role-label">"Role"</span>
            <div class="segmented" role="radiogroup" aria-labelledby="role-label">
                {option(Role::User, "Player")}
                {option(Role::Admin, "Admin")}
            </div>
            <small>
                {move || if role.get() == Role::Admin {
                    "Manages users and every server."
                } else {
                    "Manages their own servers, within their quota."
                }}
            </small>
        </div>
    }
}

#[component]
fn UsersTab() -> impl IntoView {
    let me = use_me();
    let users = polled(Some(Duration::from_secs(10)), || async {
        api::get::<Vec<UserView>>("/api/users").await
    });
    let my_name = move || {
        me.data
            .with(|u| u.as_ref().map(|u| u.username.clone()).unwrap_or_default())
    };

    // Create.
    let create_open = RwSignal::new(false);
    let new_name = RwSignal::new(String::new());
    let new_password = RwSignal::new(String::new());
    let new_role = RwSignal::new(Role::User);
    let new_quota = QuotaForm::new();
    let create_error = RwSignal::new(None::<String>);
    let open_create = move |_| {
        new_name.set(String::new());
        new_password.set(String::new());
        new_role.set(Role::User);
        new_quota.fill(&Quota::default());
        create_error.set(None);
        create_open.set(true);
    };
    let create = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let quota = match new_quota.quota() {
            Ok(q) => q,
            Err(e) => return create_error.set(Some(e)),
        };
        let body = CreateUser {
            username: new_name.get_untracked().trim().to_string(),
            password: new_password.get_untracked(),
            role: new_role.get_untracked(),
            quota,
        };
        if body.username.is_empty() || body.password.is_empty() {
            return create_error.set(Some("A user needs a name and a password.".into()));
        }
        create_error.set(None);
        spawn_local(async move {
            match api::post::<_, UserView>("/api/users", &body).await {
                Ok(u) => {
                    toasts().ok(format!("{} can now sign in", u.username));
                    create_open.set(false);
                    users.reload();
                }
                Err(e) => create_error.set(Some(e)),
            }
        });
    };

    // Edit.
    let edit_open = RwSignal::new(false);
    let editing = RwSignal::new(String::new());
    let edit_role = RwSignal::new(Role::User);
    let edit_password = RwSignal::new(String::new());
    let edit_quota = QuotaForm::new();
    let edit_error = RwSignal::new(None::<String>);
    let open_edit = move |u: &UserView| {
        editing.set(u.username.clone());
        edit_role.set(u.role);
        edit_password.set(String::new());
        edit_quota.fill(&u.quota);
        edit_error.set(None);
        edit_open.set(true);
    };
    let save = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let quota = match edit_quota.quota() {
            Ok(q) => q,
            Err(e) => return edit_error.set(Some(e)),
        };
        let password = edit_password.get_untracked();
        let body = UpdateUser {
            password: (!password.is_empty()).then_some(password),
            role: Some(edit_role.get_untracked()),
            quota: Some(quota),
        };
        let name = editing.get_untracked();
        edit_error.set(None);
        spawn_local(async move {
            let path = format!("/api/users/{}", js_sys::encode_uri_component(&name));
            match api::put::<_, UserView>(&path, &body).await {
                Ok(_) => {
                    toasts().ok(format!("{name} updated"));
                    edit_open.set(false);
                    users.reload();
                    me.reload();
                }
                Err(e) => edit_error.set(Some(e)),
            }
        });
    };

    // Delete.
    let delete_open = RwSignal::new(false);
    let deleting = RwSignal::new(None::<UserView>);
    let delete = move |_| {
        let Some(u) = deleting.get_untracked() else {
            return;
        };
        let path = format!("/api/users/{}", js_sys::encode_uri_component(&u.username));
        run(
            format!("{} deleted", u.username),
            async move { api::delete(&path).await },
            move || {
                delete_open.set(false);
                users.reload();
            },
        );
    };

    view! {
        <div class="toolbar">
            <span class="muted">{move || users.data.with(|u| u.as_ref().map(|u| plural(u.len() as u64, "user", "users")))}</span>
            <button type="button" class="button button-primary push-right" on:click=open_create>"+ New user"</button>
        </div>
        <ErrorNote error=users.error/>
        <Show when=move || users.loaded() fallback=Loading>
            <div class="table-wrap">
                <table class="table">
                    <thead>
                        <tr>
                            <th scope="col">"User"</th>
                            <th scope="col">"Memory in use"</th>
                            <th scope="col">"Servers"</th>
                            <th scope="col">"CPU"</th>
                            <th scope="col">"Disk"</th>
                            <th scope="col">"Since"</th>
                            <th scope="col"><span class="visually-hidden">"Actions"</span></th>
                        </tr>
                    </thead>
                    <tbody>
                        // Keyed by the whole user, so a row is redrawn when it changed.
                        <For each=move || users.data.get().unwrap_or_default() key=|u| format!("{u:?}") let:u>
                            {
                                let is_me = u.username == my_name();
                                let edit_user = u.clone();
                                let delete_user = u.clone();
                                view! {
                                    <tr>
                                        <td>
                                            <div class="cell-user">
                                                <span class="avatar" aria-hidden="true">{u.username.chars().next().map(String::from)}</span>
                                                <span class="strong">{u.username.clone()}</span>
                                                {(u.role == Role::Admin).then(|| view! { <span class="badge badge-lagoon">"admin"</span> })}
                                                {is_me.then(|| view! { <span class="muted small">"(you)"</span> })}
                                            </div>
                                        </td>
                                        <td style="min-width: 160px">
                                            <Meter label="Memory" percent=percent(u.usage.memory_mb, u.quota.memory_mb)
                                                detail=format!("{} / {}", mb(u.usage.memory_mb), mb(u.quota.memory_mb))/>
                                        </td>
                                        <td class="num">
                                            {format!("{} / {}", u.usage.servers, u.quota.max_servers)}
                                            <div class="muted small">{format!("{} running", u.usage.running)}</div>
                                        </td>
                                        <td class="num">{cores(u.quota.cpu_percent)}</td>
                                        <td class="num">{format!("{} / {}", mb(u.usage.disk_mb), mb(u.quota.disk_mb))}</td>
                                        <td class="num muted">{date(u.created_at)}</td>
                                        <td class="row-actions">
                                            <button type="button" class="link-button"
                                                on:click=move |_| open_edit(&edit_user)>"Edit"</button>
                                            <button type="button" class="link-button danger" disabled=is_me
                                                title=if is_me { "You can't delete yourself" } else { "" }
                                                on:click=move |_| {
                                                    deleting.set(Some(delete_user.clone()));
                                                    delete_open.set(true);
                                                }>"Delete"</button>
                                        </td>
                                    </tr>
                                }
                            }
                        </For>
                    </tbody>
                </table>
            </div>
        </Show>

        <Modal open=create_open title="New user">
            <form class="form" on:submit=create novalidate>
                <div class="form-grid">
                    <label class="field">
                        <span class="field-label">"Username"</span>
                        <input type="text" bind:value=new_name required autocomplete="off"
                            autocapitalize="none" spellcheck="false" autofocus/>
                    </label>
                    <label class="field">
                        <span class="field-label">"Password"</span>
                        <input type="password" bind:value=new_password required autocomplete="new-password"/>
                        <small>"Give it to them; they can change it from their account."</small>
                    </label>
                </div>
                <RoleField role=new_role/>
                <QuotaFields form=new_quota/>
                <ErrorNote error=create_error/>
                <div class="form-actions">
                    <button type="button" class="button" on:click=move |_| create_open.set(false)>"Cancel"</button>
                    <button type="submit" class="button button-primary">"Create user"</button>
                </div>
            </form>
        </Modal>

        <Modal open=edit_open title="Edit user">
            <form class="form" on:submit=save novalidate>
                <p class="muted">"Editing " <strong>{move || editing.get()}</strong></p>
                <RoleField role=edit_role/>
                <QuotaFields form=edit_quota/>
                <label class="field">
                    <span class="field-label">"New password"</span>
                    <input type="password" bind:value=edit_password autocomplete="new-password"/>
                    <small>"Leave empty to keep their current one."</small>
                </label>
                <p class="muted small">"A smaller quota doesn't stop servers already running."</p>
                <ErrorNote error=edit_error/>
                <div class="form-actions">
                    <button type="button" class="button" on:click=move |_| edit_open.set(false)>"Cancel"</button>
                    <button type="submit" class="button button-primary">"Save"</button>
                </div>
            </form>
        </Modal>

        <Modal open=delete_open title="Delete user?">
            {move || deleting.get().map(|u| view! {
                <p>
                    <strong>{u.username.clone()}</strong>
                    " won't be able to sign in anymore."
                </p>
                {(u.usage.servers > 0).then(|| view! {
                    <div class="note note-warning">
                        {format!(
                            "They still have {}: delete those first.",
                            plural(u.usage.servers as u64, "server", "servers"),
                        )}
                    </div>
                })}
            })}
            <div class="form-actions">
                <button type="button" class="button" on:click=move |_| delete_open.set(false)>"Cancel"</button>
                <button type="button" class="button button-danger"
                    disabled=move || deleting.with(|u| u.as_ref().is_some_and(|u| u.usage.servers > 0))
                    on:click=delete>"Delete user"</button>
            </div>
        </Modal>
    }
}

// ---------------------------------------------------------------- machine

#[component]
fn MachineTab() -> impl IntoView {
    let overview = polled(Some(Duration::from_secs(5)), || async {
        api::get::<Overview>("/api/overview").await
    });
    let servers = polled(Some(Duration::from_secs(5)), || async {
        api::get::<Vec<ServerView>>("/api/servers").await
    });
    let cpu_used = move || {
        servers.data.with(|s| {
            s.iter()
                .flatten()
                .filter_map(|s| s.usage.as_ref())
                .map(|u| u.cpu_percent as f64)
                .sum::<f64>()
        })
    };
    let ram_used = move || {
        servers.data.with(|s| {
            s.iter()
                .flatten()
                .filter_map(|s| s.usage.as_ref())
                .map(|u| u.memory_bytes)
                .sum::<u64>()
        })
    };

    view! {
        <ErrorNote error=overview.error/>
        <Show when=move || overview.loaded() fallback=Loading>
            {move || overview.data.get().map(|o| {
                let pool = (o.ports_last as u32).saturating_sub(o.ports_first as u32) + 1;
                let used_cpu = cpu_used();
                view! {
                    {if o.limits_enforced {
                        view! {
                            <div class="note note-good" role="status">
                                "✓ Each server's memory and CPU limits are enforced."
                            </div>
                        }.into_any()
                    } else {
                        view! {
                            <div class="note note-warning" role="alert">
                                <strong>"⚠ Limits are not enforced. "</strong>
                                "Quotas still decide who may start what, but a server can use more memory or CPU than it was given."
                                {o.limits_note.clone().map(|n| view! { <div class="small">{n}</div> })}
                            </div>
                        }.into_any()
                    }}
                    <div class="tiles">
                        <div class="tile">
                            <span class="tile-label">"Servers running"</span>
                            <span class="tile-value">{format!("{} / {}", o.running, o.servers)}</span>
                            <span class="tile-sub">"running / created"</span>
                        </div>
                        <div class="tile">
                            <span class="tile-label">"Ports"</span>
                            <span class="tile-value">{format!("{} / {}", o.ports_used, pool)}</span>
                            <span class="tile-sub">{format!("in use, from {} to {}", o.ports_first, o.ports_last)}</span>
                        </div>
                        <div class="tile">
                            <span class="tile-label">"Players connect to"</span>
                            <span class="tile-value" style="font-size: 16px; overflow-wrap: anywhere">
                                {o.public_host.clone().unwrap_or_else(|| crate::util::connect_address("{host}"))}
                            </span>
                            <span class="tile-sub">
                                {if o.public_host.is_some() { "the public address set for archipel" } else { "no public address set: this page's" }}
                            </span>
                        </div>
                    </div>
                    <section class="card">
                        <h2 class="section-title">"Resources"</h2>
                        <div class="meters">
                            {match o.memory_mb {
                                Some(total) => view! {
                                    <Meter label="Memory reserved" percent=percent(o.memory_used_mb, total)
                                        detail=format!("{} of {}", mb(o.memory_used_mb), mb(total))
                                        sub=format!("By running servers; they actually use {} now.", bytes(ram_used()))/>
                                }.into_any(),
                                None => view! {
                                    <Meter label="Memory reserved" percent=0.0
                                        detail=format!("{} (no limit known)", mb(o.memory_used_mb))
                                        sub=format!("By running servers; they actually use {} now.", bytes(ram_used()))/>
                                }.into_any(),
                            }}
                            {match o.cpu_percent {
                                Some(total) => view! {
                                    <Meter label="CPU in use" percent=used_cpu * 100.0 / total.max(1) as f64
                                        detail=format!("{:.1} of {}", used_cpu / 100.0, cores(total))/>
                                }.into_any(),
                                None => view! {
                                    <Meter label="CPU in use" percent=0.0
                                        detail=format!("{:.1} cores (no limit known)", used_cpu / 100.0)/>
                                }.into_any(),
                            }}
                            {match (o.disk_used_bytes, o.disk_total_bytes) {
                                (Some(used), Some(total)) => view! {
                                    <Meter label="Disk" percent=percent(used, total)
                                        detail=format!("{} of {}", bytes(used), bytes(total))/>
                                }.into_any(),
                                _ => view! { <p class="muted small">"Disk usage is not known."</p> }.into_any(),
                            }}
                        </div>
                    </section>
                }
            })}
        </Show>
    }
}
