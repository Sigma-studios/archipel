use std::future::Future;
use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;

/// Data fetched on mount, every `every`, and on `reload()`.
pub struct Polled<T: Send + Sync + 'static> {
    pub data: RwSignal<Option<T>>,
    pub error: RwSignal<Option<String>>,
    version: RwSignal<u32>,
}

impl<T: Send + Sync + 'static> Clone for Polled<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: Send + Sync + 'static> Copy for Polled<T> {}

impl<T: Send + Sync + 'static> Polled<T> {
    pub fn reload(&self) {
        self.version.update(|v| *v += 1);
    }

    pub fn loaded(&self) -> bool {
        self.data.with(Option::is_some)
    }
}

/// `fetch` is called inside an effect: signals it reads before its first `.await`
/// are tracked, so the data reloads when they change (e.g. a route parameter).
pub fn polled<T, F, Fut>(every: Option<Duration>, fetch: F) -> Polled<T>
where
    T: Send + Sync + 'static,
    F: Fn() -> Fut + 'static,
    Fut: Future<Output = Result<T, String>> + 'static,
{
    let polled = Polled {
        data: RwSignal::new(None),
        error: RwSignal::new(None),
        version: RwSignal::new(0),
    };
    Effect::new(move |_| {
        polled.version.track();
        let future = fetch();
        spawn_local(async move {
            match future.await {
                Ok(value) => {
                    polled.data.try_set(Some(value));
                    polled.error.try_set(None);
                }
                Err(e) => {
                    polled.error.try_set(Some(e));
                }
            }
        });
    });
    if let Some(every) = every
        && let Ok(handle) = set_interval_with_handle(move || polled.reload(), every)
    {
        on_cleanup(move || handle.clear());
    }
    polled
}

// ---------------------------------------------------------------- toasts

#[derive(Clone, PartialEq)]
pub struct Toast {
    pub id: u64,
    pub message: String,
    pub error: bool,
}

#[derive(Clone, Copy)]
pub struct Toasts(pub RwSignal<Vec<Toast>>);

thread_local! {
    // Global rather than a context: toasts are raised from async tasks, which run
    // outside any reactive owner.
    static TOASTS: Toasts = Toasts(RwSignal::new(vec![]));
}

pub fn toasts() -> Toasts {
    TOASTS.with(|t| *t)
}

impl Toasts {
    fn push(&self, message: String, error: bool) {
        let id = (js_sys::Math::random() * 1e15) as u64;
        self.0.update(|t| t.push(Toast { id, message, error }));
        let list = self.0;
        set_timeout(
            move || list.update(|t| t.retain(|x| x.id != id)),
            Duration::from_millis(if error { 8000 } else { 3500 }),
        );
    }

    pub fn ok(&self, message: impl Into<String>) {
        self.push(message.into(), false);
    }

    pub fn err(&self, message: impl Into<String>) {
        self.push(message.into(), true);
    }

    pub fn dismiss(&self, id: u64) {
        self.0.update(|t| t.retain(|x| x.id != id));
    }
}

/// Runs an API call, reporting success or failure as a toast.
pub fn run<Fut>(success: impl Into<String>, future: Fut, then: impl FnOnce() + 'static)
where
    Fut: Future<Output = Result<(), String>> + 'static,
{
    let toasts = toasts();
    let success = success.into();
    spawn_local(async move {
        match future.await {
            Ok(()) => {
                if !success.is_empty() {
                    toasts.ok(success);
                }
                then();
            }
            Err(e) => toasts.err(e),
        }
    });
}

// ---------------------------------------------------------------- formatting

pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Memory or disk given in MB, as people say it: `512 MB`, `2 GB`, `1.5 GB`.
pub fn mb(n: u64) -> String {
    if n < 1024 {
        return format!("{n} MB");
    }
    let gb = n as f64 / 1024.0;
    if (gb - gb.round()).abs() < 0.05 {
        format!("{gb:.0} GB")
    } else {
        format!("{gb:.1} GB")
    }
}

/// CPU in percent of one core, as cores: `2 cores`, `half a core`.
pub fn cores(percent: u32) -> String {
    match percent {
        50 => "half a core".into(),
        100 => "1 core".into(),
        p if p % 100 == 0 => format!("{} cores", p / 100),
        p => format!("{:.1} cores", p as f64 / 100.0),
    }
}

pub fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

pub fn now() -> u64 {
    (js_sys::Date::now() / 1000.0) as u64
}

/// How long since `at`, for uptimes: `3m`, `2h 10m`, `4d 2h`.
pub fn since(at: u64) -> String {
    let secs = now().saturating_sub(at);
    match secs {
        0..60 => "less than a minute".into(),
        60..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d {}h", secs / 86400, secs % 86400 / 3600),
    }
}

/// Local date, `2027-01-18`.
pub fn date(at: u64) -> String {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(at as f64 * 1000.0));
    format!(
        "{:04}-{:02}-{:02}",
        date.get_full_year(),
        date.get_month() + 1,
        date.get_date()
    )
}

pub fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 * 100.0 / whole as f64
    }
}

/// Removes ANSI colour codes that game servers write to their consoles.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn copy_to_clipboard(text: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.navigator().clipboard().write_text(text);
    }
}

/// A server's connect address with `{host}` filled in with the address this page
/// is browsed at (hosted mode; on the desktop the server fills it in itself).
pub fn connect_address(template: &str) -> String {
    let browsed = || {
        web_sys::window()
            .and_then(|w| w.location().hostname().ok())
            .filter(|h| !h.is_empty())
    };
    let host = (!desktop())
        .then(browsed)
        .flatten()
        .unwrap_or_else(|| "localhost".into());
    template.replace("{host}", &host)
}

/// One of six tints for a game's badge, the same for a game everywhere.
pub fn tint(game: &str) -> String {
    let hash = game
        .bytes()
        .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    format!("tint-{}", hash % 6)
}

// ---------------------------------------------------------------- session

/// The signed-in user, fetched by the page layout and shared with every page.
pub type Me = Polled<archipel_core::api::UserView>;

pub fn use_me() -> Me {
    expect_context::<Me>()
}

/// Admin pages and other people's servers: hosted mode only (on the desktop the
/// one user owns everything, and there is nobody to manage).
pub fn is_admin(me: Me) -> bool {
    !desktop()
        && me.data.with(|u| {
            u.as_ref()
                .is_some_and(|u| u.role == archipel_core::api::Role::Admin)
        })
}

// ---------------------------------------------------------------- mode

thread_local! {
    // Asked once from `GET /api/about` before any page shows, and fixed after.
    static MODE: std::cell::Cell<archipel_core::api::Mode> =
        const { std::cell::Cell::new(archipel_core::api::Mode::Hosted) };
}

pub fn set_mode(mode: archipel_core::api::Mode) {
    MODE.with(|m| m.set(mode));
}

/// The desktop app: no login, one owner, the quota is this PC.
pub fn desktop() -> bool {
    MODE.with(|m| m.get()) == archipel_core::api::Mode::Desktop
}
