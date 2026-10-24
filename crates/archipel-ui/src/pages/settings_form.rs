//! A game's settings form, built from its definition: one field per `SettingDef`,
//! drawn after its kind. Values live in one map, as the API takes them.

use std::collections::BTreeMap;

use archipel_core::api::Value;
use archipel_core::game::{ChoiceOption, GameDef, SettingDef, SettingKind};
use leptos::prelude::*;

use crate::api;
use crate::util::{mb, polled};

pub type Values = BTreeMap<String, Value>;

/// Every setting at its default.
pub fn defaults(game: &GameDef) -> Values {
    game.settings
        .iter()
        .map(|s| (s.key.clone(), s.default_value()))
        .collect()
}

/// Choices this short fit side by side; longer ones go in a select.
fn segmented(options: &[archipel_core::game::ChoiceOption]) -> bool {
    options.len() <= 4 && options.iter().all(|o| o.label.chars().count() <= 12)
}

#[component]
pub fn SettingsFields(
    game: GameDef,
    values: RwSignal<Values>,
    /// On an existing server: settings that reinstall say so.
    #[prop(optional)]
    editing: bool,
    /// The server edited, whose own files (maps) some choices list.
    #[prop(optional)]
    server: Option<u64>,
) -> impl IntoView {
    if game.settings.is_empty() {
        return view! { <p class="muted">"This game has nothing to set up."</p> }.into_any();
    }
    // Listed options may depend on the other choices (Paper builds fewer versions).
    let choices: Vec<String> = game
        .settings
        .iter()
        .filter(|s| matches!(s.kind, SettingKind::Choice { .. }))
        .map(|s| s.key.clone())
        .collect();
    let game_id = game.id.clone();
    view! {
        <div class="form-grid">
            {game.settings.into_iter().map(|def| {
                let (game_id, choices) = (game_id.clone(), choices.clone());
                view! { <SettingField def values editing game_id choices server/> }
            }).collect_view()}
        </div>
    }
    .into_any()
}

#[component]
fn SettingField(
    def: SettingDef,
    values: RwSignal<Values>,
    editing: bool,
    game_id: String,
    choices: Vec<String>,
    server: Option<u64>,
) -> impl IntoView {
    let id = format!("setting-{}", def.key);
    let key = StoredValue::new(def.key.clone());
    let fallback = StoredValue::new(def.default_value());
    let current = move || {
        values
            .with(|v| key.with_value(|k| v.get(k).cloned()))
            .unwrap_or_else(|| fallback.get_value())
    };
    let set = move |value: Value| {
        values.update(|v| {
            v.insert(key.get_value(), value);
        })
    };
    let required = def.required;
    let label = view! {
        <label class="field-label" for=id.clone()>
            {def.label.clone()}
            {required.then(|| view! { <span class="required" aria-hidden="true">"*"</span> })}
        </label>
    };
    let help = (!def.help.is_empty())
        .then(|| view! { <small class="field-help">{def.help.clone()}</small> });
    let reinstall = (editing && def.reinstall).then(|| {
        view! {
            <small class="reinstall-note">
                "↻ Changing this downloads the game files again at the next start."
            </small>
        }
    });
    let link = def.link.clone().map(|href| {
        view! {
            <small class="field-help">
                <a href=href target="_blank" rel="noopener noreferrer">"Read it ↗"</a>
            </small>
        }
    });

    match def.kind {
        SettingKind::Text { max_len, .. } => view! {
            <div class="field">
                {label}
                <input type="text" id=id maxlength=max_len.map(|m| m.to_string()) required=required
                    prop:value=move || current().as_text()
                    on:input=move |ev| set(Value::Text(event_target_value(&ev)))/>
                {help}{link}{reinstall}
            </div>
        }
        .into_any(),
        SettingKind::Secret { .. } => {
            let shown = RwSignal::new(false);
            view! {
                <div class="field">
                    {label}
                    <div class="secret-input">
                        <input id=id type=move || if shown.get() { "text" } else { "password" }
                            autocomplete="new-password" required=required
                            prop:value=move || current().as_text()
                            on:input=move |ev| set(Value::Text(event_target_value(&ev)))/>
                        <button type="button" class="button button-small"
                            aria-pressed=move || shown.get().to_string()
                            on:click=move |_| shown.update(|s| *s = !*s)>
                            {move || if shown.get() { "Hide" } else { "Show" }}
                        </button>
                    </div>
                    {help}{link}{reinstall}
                </div>
            }
            .into_any()
        }
        SettingKind::Number { min, max, .. } => {
            let range = match (min, max) {
                (Some(a), Some(b)) => format!("From {a} to {b}."),
                (Some(a), None) => format!("At least {a}."),
                (None, Some(b)) => format!("At most {b}."),
                (None, None) => String::new(),
            };
            view! {
                <div class="field">
                    {label}
                    <input type="number" id=id step="1" inputmode="numeric" required=true
                        min=min.map(|m| m.to_string()) max=max.map(|m| m.to_string())
                        prop:value=move || current().as_text()
                        on:input=move |ev| {
                            let raw = event_target_value(&ev);
                            set(raw.trim().parse().map(Value::Number).unwrap_or(Value::Text(raw)))
                        }/>
                    {help}
                    {(!range.is_empty()).then(|| view! { <small class="field-help">{range}</small> })}
                    {link}{reinstall}
                </div>
            }
            .into_any()
        }
        SettingKind::Toggle { .. } => view! {
            <div class="switch-field field-wide">
                <label class=if required { "switch switch-required" } else { "switch" }>
                    <input type="checkbox" role="switch" id=id required=required
                        prop:checked=move || current().as_bool()
                        on:change=move |ev| set(Value::Bool(event_target_checked(&ev)))/>
                    <span>
                        {def.label.clone()}
                        {required.then(|| view! { <span class="required" aria-hidden="true">"*"</span> })}
                    </span>
                </label>
                {help}{link}{reinstall}
            </div>
        }
        .into_any(),
        SettingKind::Choice {
            options,
            options_from: Some(_),
            ..
        } => {
            // archipel lists the options, after the other choices made so far.
            let path = StoredValue::new(format!("/api/games/{game_id}/options/{}", def.key));
            let others = StoredValue::new(choices);
            // The other choices the list was first asked with: once they change
            // (another gamemode), a value the new list lacks gives way to its first.
            let first_query = StoredValue::new(None::<String>);
            let others_changed = StoredValue::new(false);
            let listed = polled(None, move || {
                let mut query = values.with(|v| {
                    others.with_value(|keys| {
                        keys.iter()
                            .filter(|k| key.with_value(|me| *k != me))
                            .filter_map(|k| v.get(k).map(|val| (k, val.as_text())))
                            .map(|(k, val)| format!("{}={}", encode(k), encode(&val)))
                            .collect::<Vec<_>>()
                    })
                });
                query.extend(server.map(|id| format!("_server={id}")));
                let query = query.join("&");
                match first_query.get_value() {
                    None => first_query.set_value(Some(query.clone())),
                    Some(first) if first != query => others_changed.set_value(true),
                    Some(_) => {}
                }
                let url = format!("{}?{query}", path.get_value());
                async move { api::get::<Vec<ChoiceOption>>(&url).await }
            });
            Effect::new(move |_| {
                let Some(list) = listed.data.get() else { return };
                if !others_changed.get_value() {
                    return;
                }
                let now = values
                    .with_untracked(|v| key.with_value(|k| v.get(k).map(Value::as_text)))
                    .unwrap_or_default();
                if !list.iter().any(|o| o.value == now)
                    && let Some(first) = list.first()
                {
                    set(Value::Text(first.value.clone()));
                }
            });
            let fallback_options = StoredValue::new(options);
            let shown = move || {
                let mut all = listed
                    .data
                    .get()
                    .unwrap_or_else(|| fallback_options.get_value());
                // The current value stays choosable, listed or not (yet).
                let now = current().as_text();
                if !all.iter().any(|o| o.value == now) {
                    all.insert(0, ChoiceOption { label: now.clone(), value: now });
                }
                all
            };
            view! {
                <div class="field">
                    {label}
                    <select id=id prop:value=move || current().as_text()
                        on:change=move |ev| set(Value::Text(event_target_value(&ev)))>
                        {move || shown().into_iter().map(|o| {
                            let value = o.value.clone();
                            view! {
                                <option value=o.value selected=move || current().as_text() == value>{o.label}</option>
                            }
                        }).collect_view()}
                    </select>
                    {move || listed.error.get().map(|_| view! {
                        <small class="field-help">"The full list is unavailable right now."</small>
                    })}
                    {help}{link}{reinstall}
                </div>
            }
            .into_any()
        }
        SettingKind::Choice { options, .. } if segmented(&options) => {
            let label_id = format!("{id}-label");
            view! {
                <div class="field field-segmented">
                    <span class="field-label" id=label_id.clone()>{def.label.clone()}</span>
                    <div class="segmented" role="radiogroup" aria-labelledby=label_id>
                        {options.into_iter().map(|o| {
                            let value = StoredValue::new(o.value);
                            let active = move || value.with_value(|v| current().as_text() == *v);
                            view! {
                                <button type="button" role="radio"
                                    class=move || if active() { "segment active" } else { "segment" }
                                    aria-checked=move || active().to_string()
                                    on:click=move |_| set(Value::Text(value.get_value()))>
                                    {o.label}
                                </button>
                            }
                        }).collect_view()}
                    </div>
                    {help}{link}{reinstall}
                </div>
            }
            .into_any()
        }
        SettingKind::Choice { options, .. } => view! {
            <div class="field">
                {label}
                <select id=id prop:value=move || current().as_text()
                    on:change=move |ev| set(Value::Text(event_target_value(&ev)))>
                    {options.into_iter().map(|o| {
                        let value = o.value.clone();
                        view! {
                            <option value=o.value selected=move || current().as_text() == value>{o.label}</option>
                        }
                    }).collect_view()}
                </select>
                {help}{link}{reinstall}
            </div>
        }
        .into_any(),
    }
}

fn encode(s: &str) -> String {
    String::from(js_sys::encode_uri_component(s))
}

/// Memory as a slider from `min` to `max`, in 128 MB steps.
#[component]
pub fn MemoryField(
    value: RwSignal<u64>,
    #[prop(into)] min: Signal<u64>,
    #[prop(into)] max: Signal<u64>,
    default_mb: u64,
    #[prop(into)] help: Signal<String>,
) -> impl IntoView {
    view! {
        <div class="range-field field-wide">
            <label class="field-label" for="memory">"Memory"</label>
            <div class="range-row">
                <input type="range" id="memory" step="128"
                    min=move || min.get().to_string() max=move || max.get().to_string()
                    prop:value=move || value.get().to_string()
                    aria-valuetext=move || mb(value.get())
                    on:input=move |ev| {
                        if let Ok(v) = event_target_value(&ev).parse::<u64>() {
                            value.set(v);
                        }
                    }/>
                <output class="range-value" for="memory">{move || mb(value.get())}</output>
            </div>
            <div class="range-ends">
                <span>{move || mb(min.get())}</span>
                <Show when=move || { value.get() != default_mb && (min.get()..=max.get()).contains(&default_mb) }>
                    <button type="button" class="link-button small" on:click=move |_| value.set(default_mb)>
                        {format!("Use the default ({})", mb(default_mb))}
                    </button>
                </Show>
                <span>{move || mb(max.get())}</span>
            </div>
            <small class="field-help">{move || help.get()}</small>
        </div>
    }
}
