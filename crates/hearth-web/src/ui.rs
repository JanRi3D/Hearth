//! The building blocks every page uses. Class names come from the prototype's stylesheet
//! (crates/hearth/ui/app.css), so the pages look like the design without restating it.

use std::{cell::Cell, time::Duration};

use hearth_api::Id;
use leptos::{prelude::*, task::spawn_local};
use wasm_bindgen::{JsCast, JsValue};

use crate::icons;

// ---------- icons ----------

#[component]
pub fn Icon(name: &'static str, #[prop(default = 16)] size: u32, #[prop(default = 1.75)] stroke: f64) -> impl IntoView {
    view! {
        <svg
            width=size.to_string()
            height=size.to_string()
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width=stroke.to_string()
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
        >
            <path d=icons::path(name)></path>
        </svg>
    }
}

/// The serif wordmark with the Bonfire mark in ember.
#[component]
pub fn Brand(#[prop(into)] text: String) -> impl IntoView {
    view! { <span class="brand"><Icon name="mark" size=20 stroke=2.0/>{text}</span> }
}

/// A round initial on a colour derived from the name, as the prototype draws people.
#[component]
pub fn Avatar(#[prop(into)] name: String, #[prop(default = 28)] size: u32) -> impl IntoView {
    let hue = name.bytes().fold(7u32, |hash, byte| hash.wrapping_mul(31).wrapping_add(byte as u32)) % 360;
    let initial = name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    view! { <span class="av" style=format!("--s:{size}px;--h:{hue}")>{initial}</span> }
}

// ---------- form controls ----------

/// What a control does on change when nobody asked to be told.
pub fn noop() -> Callback<()> {
    Callback::new(|_| {})
}

#[component]
pub fn Toggle(
    checked: RwSignal<bool>,
    #[prop(into)] label: String,
    #[prop(default = noop(), into)] on_commit: Callback<()>,
) -> impl IntoView {
    let flip = move |_| {
        checked.update(|on| *on = !*on);
        on_commit.run(());
    };
    view! { <button type="button" class="sw" role="switch" aria-checked=move || checked.get().to_string() aria-label=label on:click=flip></button> }
}

/// A text field bound to a signal. `on_commit` fires when the person leaves the field or
/// presses Enter, which is when a settings page saves.
#[component]
pub fn TextInput(
    value: RwSignal<String>,
    #[prop(optional, into)] id: Option<String>,
    #[prop(optional, into)] label: Option<String>,
    #[prop(optional, into)] placeholder: Signal<String>,
    #[prop(optional, into)] class: String,
    #[prop(optional, into)] style: String,
    #[prop(default = "text")] kind: &'static str,
    #[prop(default = "off")] autocomplete: &'static str,
    #[prop(default = noop(), into)] on_commit: Callback<()>,
) -> impl IntoView {
    let label = label.unwrap_or_else(|| placeholder.get_untracked());
    view! {
        <input
            class=format!("input {class}")
            type=kind
            id=id
            aria-label=label
            placeholder=move || placeholder.get()
            style=style
            autocomplete=autocomplete
            spellcheck="false"
            prop:value=move || value.get()
            on:input=move |ev| value.set(event_target_value(&ev))
            on:change=move |_| on_commit.run(())
        />
    }
}

/// A select over a fixed list of values.
#[component]
pub fn Choice<T>(
    value: RwSignal<T>,
    options: Vec<(T, &'static str)>,
    #[prop(into)] label: String,
    #[prop(optional, into)] id: Option<String>,
    #[prop(optional, into)] style: String,
    #[prop(optional)] disabled: bool,
    #[prop(default = noop(), into)] on_commit: Callback<()>,
) -> impl IntoView
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    let choices = StoredValue::new(options.clone());
    let pick = move |ev| {
        let index: usize = event_target_value(&ev).parse().unwrap_or(0);
        if let Some((picked, _)) = choices.with_value(|choices| choices.get(index).cloned()) {
            value.set(picked);
            on_commit.run(());
        }
    };
    view! {
        <select class="input" id=id aria-label=label style=style disabled=disabled on:change=pick>
            {options
                .into_iter()
                .enumerate()
                .map(|(index, (option, text))| {
                    view! { <option value=index.to_string() prop:selected=move || value.get() == option>{text}</option> }
                })
                .collect_view()}
        </select>
    }
}

/// A labelled field, stacked: label above, control below.
#[component]
pub fn Field(#[prop(into)] label: String, #[prop(optional, into)] target: Option<String>, children: Children) -> impl IntoView {
    view! { <div class="field"><label for=target>{label}</label>{children()}</div> }
}

/// One line of a settings card: what it is on the left, the control on the right.
#[component]
pub fn Srow(#[prop(into)] title: String, #[prop(optional, into)] desc: MaybeProp<String>, children: Children) -> impl IntoView {
    view! {
        <div class="srow">
            <div class="col" style="gap:1px">
                <span style="font-weight:500">{title}</span>
                {move || desc.get().filter(|desc| !desc.is_empty()).map(|desc| view! { <span class="muted" style="font-size:12px">{desc}</span> })}
            </div>
            <div class="hrow" style="justify-content:flex-end">{children()}</div>
        </div>
    }
}

/// A settings row whose control is a switch.
#[component]
pub fn Trow(
    #[prop(into)] title: String,
    #[prop(optional, into)] desc: MaybeProp<String>,
    checked: RwSignal<bool>,
    #[prop(default = noop(), into)] on_commit: Callback<()>,
) -> impl IntoView {
    let label = title.clone();
    view! { <Srow title=title desc=desc><Toggle checked=checked label=label on_commit=on_commit/></Srow> }
}

/// A titled card of rows.
#[component]
pub fn Group(#[prop(into)] title: String, #[prop(optional, into)] note: Option<String>, children: Children) -> impl IntoView {
    view! {
        <section class="sec">
            <h2 class="sh">{title}</h2>
            {note.map(|note| view! { <p class="muted" style="font-size:12px;max-width:72ch">{note}</p> })}
            <div class="card rows">{children()}</div>
        </section>
    }
}

/// The warning line under a form.
#[component]
pub fn ErrorNote(message: RwSignal<String>) -> impl IntoView {
    view! {
        <Show when=move || !message.get().is_empty()>
            <div class="note warn" role="alert"><Icon name="alert" size=14/><span>{move || message.get()}</span></div>
        </Show>
    }
}

#[component]
pub fn Dialog(
    #[prop(into)] title: String,
    on_close: Callback<()>,
    #[prop(optional, into)] style: String,
    /// Sits above the setup wizard, which is itself a layer.
    #[prop(optional)]
    over: bool,
    children: Children,
) -> impl IntoView {
    let label = title.clone();
    view! {
        <div class="scrim" class:over=over on:click=move |_| on_close.run(())>
            <div class="dlg" role="dialog" aria-modal="true" aria-label=label style=style on:click=|ev| ev.stop_propagation()>
                <header>
                    <h2>{title}</h2>
                    <button type="button" class="ib" aria-label="Close" on:click=move |_| on_close.run(())><Icon name="x"/></button>
                </header>
                {children()}
            </div>
        </div>
    }
}

/// Asks before something that cannot be taken back.
#[component]
pub fn Confirm(
    #[prop(into)] title: String,
    #[prop(into)] text: String,
    /// What the button that goes ahead says.
    action: &'static str,
    on_confirm: Callback<()>,
    on_close: Callback<()>,
) -> impl IntoView {
    view! {
        <Dialog title=title on_close=on_close>
            <div class="db"><p class="muted">{text}</p></div>
            <footer>
                <button type="button" class="btn" on:click=move |_| on_close.run(())>"Cancel"</button>
                <button type="button" class="btn danger" on:click=move |_| on_confirm.run(())>{action}</button>
            </footer>
        </Dialog>
    }
}

/// A select over things that are known by their id: games, PCs, emulators. With nothing
/// picked it shows the first one, and `picked` says which that is.
#[component]
pub fn Pick(id: &'static str, value: RwSignal<Option<Id>>, #[prop(into)] options: Signal<Vec<(Id, String)>>) -> impl IntoView {
    view! {
        <select class="input" id=id on:change=move |ev| value.set(event_target_value(&ev).parse().ok())>
            {move || {
                options
                    .get()
                    .into_iter()
                    .map(|(option, text)| view! { <option value=option.to_string() prop:selected=move || value.get() == Some(option)>{text}</option> })
                    .collect_view()
            }}
        </select>
    }
}

/// What a `Pick` shows: the one picked if it is still listed, otherwise the first.
pub fn picked(value: RwSignal<Option<Id>>, options: &[(Id, String)]) -> Option<Id> {
    value.get_untracked().filter(|id| options.iter().any(|(option, _)| option == id)).or(options.first().map(|(id, _)| *id))
}

/// A value to hand to someone, with a Copy button.
#[component]
pub fn CodeBox(#[prop(into)] text: Signal<String>) -> impl IntoView {
    view! {
        <div class="code">
            <span>{move || text.get()}</span>
            <button type="button" class="btn sm" on:click=move |_| copy(&text.get_untracked())>"Copy"</button>
        </div>
    }
}

// ---------- toasts ----------

#[derive(Clone, PartialEq)]
pub struct Toast {
    id: u32,
    kind: &'static str,
    title: String,
    text: String,
}

thread_local! {
    static TOASTS: Cell<Option<RwSignal<Vec<Toast>>>> = const { Cell::new(None) };
    static NEXT_TOAST: Cell<u32> = const { Cell::new(0) };
}

/// Shows a short message in the corner: what happened, and what to do next. `kind` is "ok" or "warn".
pub fn toast(kind: &'static str, title: impl Into<String>, text: impl Into<String>) {
    let Some(toasts) = TOASTS.get() else { return };
    let id = NEXT_TOAST.replace(NEXT_TOAST.get() + 1);
    toasts.update(|list| {
        list.push(Toast { id, kind, title: title.into(), text: text.into() });
        if list.len() > 4 {
            list.remove(0);
        }
    });
    set_timeout(move || toasts.update(|list| list.retain(|toast| toast.id != id)), std::time::Duration::from_millis(5200));
}

/// For a failed request: the server's own words, which say what to do next.
pub fn toast_err(title: impl Into<String>, err: &crate::api::ApiErr) {
    toast("warn", title, err.message.clone());
}

#[component]
pub fn Toasts() -> impl IntoView {
    let toasts = RwSignal::new(Vec::<Toast>::new());
    TOASTS.set(Some(toasts));
    view! {
        <div id="toasts" aria-live="polite">
            <For each=move || toasts.get() key=|toast| toast.id let:toast>
                <div class=format!("toast {}", toast.kind) role="status">
                    <span class="tico"><Icon name={ if toast.kind == "warn" { "alert" } else { "check" } } size=15/></span>
                    <div><b>{toast.title}</b>{(!toast.text.is_empty()).then(|| view! { <p>{toast.text}</p> })}</div>
                    <button
                        class="ib"
                        aria-label="Dismiss"
                        style="width:22px;height:22px"
                        on:click=move |_| toasts.update(|list| list.retain(|other| other.id != toast.id))
                    >
                        <Icon name="x" size=13/>
                    </button>
                </div>
            </For>
        </div>
    }
}

// ---------- lists a page reads and changes ----------

/// Reads a list from the Hearth into a signal, or says why that did not work.
pub fn load_list<T>(path: &'static str, into: RwSignal<Vec<T>>, failed: &'static str)
where
    T: serde::de::DeserializeOwned + Send + Sync + 'static,
{
    spawn_local(async move {
        match crate::api::get::<Vec<T>>(path).await {
            Ok(found) => into.set(found),
            Err(err) => toast_err(failed, &err),
        }
    });
}

/// Deletes something on the Hearth, then runs `then`, which usually reads a list again.
pub fn delete_then(path: String, failed: &'static str, then: impl FnOnce() + 'static) {
    spawn_local(async move {
        match crate::api::delete::<()>(&path).await {
            Ok(()) => then(),
            Err(err) => toast_err(failed, &err),
        }
    });
}

/// Runs `tick` every so many seconds for as long as the page that asked is open.
pub fn every(seconds: u64, tick: impl Fn() + 'static) {
    let timer = set_interval_with_handle(tick, Duration::from_secs(seconds)).ok();
    on_cleanup(move || {
        if let Some(timer) = timer {
            timer.clear();
        }
    });
}

// ---------- small helpers ----------

/// Copies text. The clipboard API needs HTTPS, and setup runs over plain HTTP, so there is a
/// fallback that works everywhere.
pub fn copy(text: &str) {
    let window = web_sys::window().expect("a browser window");
    let clipboard = js_sys::Reflect::get(&window.navigator(), &JsValue::from_str("clipboard")).unwrap_or(JsValue::UNDEFINED);
    if !clipboard.is_undefined() && window.is_secure_context() {
        let _ = window.navigator().clipboard().write_text(text);
        toast("ok", "Copied", "");
        return;
    }
    let copied = (|| {
        let document = window.document()?;
        let area = document.create_element("textarea").ok()?.dyn_into::<web_sys::HtmlTextAreaElement>().ok()?;
        area.set_value(text);
        area.set_attribute("style", "position:fixed;opacity:0").ok()?;
        document.body()?.append_child(&area).ok()?;
        area.select();
        let done = document.dyn_ref::<web_sys::HtmlDocument>()?.exec_command("copy").unwrap_or(false);
        area.remove();
        done.then_some(())
    })();
    match copied {
        Some(()) => toast("ok", "Copied", ""),
        None => toast("warn", "Could not copy", "Select the text and copy it by hand."),
    }
}

pub async fn sleep(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let _ = web_sys::window().expect("a browser window").set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// "166 GB", "1.4 GB", "38 MB".
pub fn size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = bytes as f64 / MB;
    if mb >= 10.0 * 1024.0 {
        format!("{:.0} GB", mb / 1024.0)
    } else if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else if mb >= 1.0 {
        format!("{mb:.0} MB")
    } else {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    }
}

/// A size for a figure that shows its unit small: ("1.4", "GB").
pub fn size_parts(bytes: u64) -> (String, String) {
    let text = size(bytes);
    let (number, unit) = text.split_once(' ').unwrap_or((&text, ""));
    (number.to_string(), unit.to_string())
}

/// "12 days", "3 h", "5 min".
pub fn duration(seconds: u64) -> String {
    match seconds {
        s if s >= 2 * 86400 => format!("{} days", s / 86400),
        s if s >= 3600 => format!("{} h", s / 3600),
        s => format!("{} min", s / 60),
    }
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// A time of day for today, a date for anything older, in the browser's own time zone.
pub fn when(at: i64) -> String {
    let date = js_sys::Date::new(&JsValue::from_f64(at as f64 * 1000.0));
    let today = js_sys::Date::new_0();
    if date.to_date_string() == today.to_date_string() {
        format!("{:02}:{:02}", date.get_hours(), date.get_minutes())
    } else {
        format!("{} {}", date.get_date(), MONTHS[date.get_month() as usize % 12])
    }
}

/// The day and the time of day, "14 Nov · 20:00", in the browser's own time zone.
pub fn day_time(at: i64) -> String {
    let date = js_sys::Date::new(&JsValue::from_f64(at as f64 * 1000.0));
    format!("{} {} · {:02}:{:02}", date.get_date(), MONTHS[date.get_month() as usize % 12], date.get_hours(), date.get_minutes())
}
