//! Signing in to the admin pages, the note people get who are not admins, and the page
//! behind a password reset link.

use hearth_api::{
    ErrorCode,
    auth::{DeviceInfo, LoginRequest, LoginResponse, ResetPassword},
    people::ResetInfo,
};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    App, api,
    ui::{Brand, ErrorNote, Field, TextInput},
};

/// The browser counts as a device, like a PC with Bonfire on it. Its id is kept so that
/// signing in again does not add a second entry under "Signed-in devices".
fn this_browser() -> DeviceInfo {
    const KEY: &str = "hearth-device";
    let window = window();
    let storage = window.local_storage().ok().flatten();
    let client_id = storage.as_ref().and_then(|storage| storage.get_item(KEY).ok().flatten()).unwrap_or_else(|| {
        let id = format!("web-{:08x}{:08x}", (js_sys::Math::random() * 4294967295.0) as u32, (js_sys::Math::random() * 4294967295.0) as u32);
        if let Some(storage) = &storage {
            let _ = storage.set_item(KEY, &id);
        }
        id
    });
    let agent = window.navigator().user_agent().unwrap_or_default();
    let browser = [("Firefox", "Firefox"), ("Edg/", "Edge"), ("Chrome", "Chrome"), ("Safari", "Safari")]
        .iter()
        .find(|(marker, _)| agent.contains(marker))
        .map_or("Browser", |(_, name)| name);
    DeviceInfo { client_id, name: "Browser".into(), platform: browser.into(), client: "Web admin".into() }
}

#[component]
pub fn Login() -> impl IntoView {
    let app = expect_context::<App>();
    let (user, pass, code) = (RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()));
    let needs_code = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if user.get_untracked().trim().is_empty() || pass.get_untracked().is_empty() {
            return error.set("Enter your username and password.".into());
        }
        let request = LoginRequest {
            username: user.get_untracked(),
            password: pass.get_untracked(),
            totp: needs_code.get_untracked().then(|| code.get_untracked()),
            device: this_browser(),
            cookie: true,
        };
        busy.set(true);
        spawn_local(async move {
            match api::post::<LoginResponse>("/auth/login", &request).await {
                Ok(_) => app.restart(),
                Err(err) if err.code == ErrorCode::TotpRequired => {
                    needs_code.set(true);
                    error.set(String::new());
                }
                Err(err) => error.set(err.message),
            }
            busy.set(false);
        });
    };

    view! {
        <div class="lock">
            <form class="onb" on:submit=submit>
                <div class="hrow"><Brand text="Hearth"/></div>
                <h1>{move || format!("Sign in to {}", app.name())}</h1>
                <p class="muted">"These pages are for the people who run this Hearth. To play, open Bonfire and add this Hearth there."</p>
                <Field label="Username" target="li-user"><TextInput value=user id="li-user" autocomplete="username"/></Field>
                <Field label="Password" target="li-pass"><TextInput value=pass id="li-pass" kind="password" autocomplete="current-password"/></Field>
                <Show when=move || needs_code.get()>
                    <Field label="Code from your authenticator app" target="li-code">
                        <TextInput value=code id="li-code" class="mono" placeholder="6 digits" autocomplete="one-time-code"/>
                    </Field>
                </Show>
                <ErrorNote message=error/>
                <div class="hrow" style="justify-content:flex-end;margin-top:4px">
                    <button class="btn pri" disabled=move || busy.get()>"Sign in"</button>
                </div>
            </form>
        </div>
    }
}

/// Shown to someone who signed in but does not run this Hearth.
#[component]
pub fn NotAdmin() -> impl IntoView {
    let app = expect_context::<App>();
    view! {
        <div class="lock">
            <div class="onb">
                <div class="hrow"><Brand text="Hearth"/></div>
                <h1>"Nothing to do here"</h1>
                <p class="muted">
                    {move || {
                        format!(
                            "You are signed in as {}. These pages are for the admins of {}. To play, open Bonfire and add this Hearth there.",
                            app.me.get().map(|me| me.user.username).unwrap_or_default(),
                            app.name()
                        )
                    }}
                </p>
                <div class="hrow" style="justify-content:flex-end"><button class="btn" on:click=move |_| app.sign_out()>"Sign out"</button></div>
            </div>
        </div>
    }
}

/// Where a password reset link leads. It works without being signed in: the link is the proof.
#[component]
pub fn Reset() -> impl IntoView {
    let app = expect_context::<App>();
    let token = app.path.get_untracked().strip_prefix("/reset/").unwrap_or_default().trim_end_matches('/').to_string();
    // Who the link is for, or the Hearth's words about why it does not work.
    let link = RwSignal::new(None::<Result<String, String>>);
    let (pass, pass2) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let done = RwSignal::new(false);
    {
        let path = format!("/auth/reset/{}", api::encode(&token));
        spawn_local(async move {
            let found = api::get::<ResetInfo>(&path).await;
            link.set(Some(found.map(|info| info.username).map_err(|err| err.message)));
        });
    }

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let new_password = pass.get_untracked();
        if new_password.chars().count() < 8 {
            return error.set("Use at least 8 characters for the password.".into());
        }
        if new_password != pass2.get_untracked() {
            return error.set("The two passwords are not the same.".into());
        }
        let request = ResetPassword { token: token.clone(), new_password };
        busy.set(true);
        spawn_local(async move {
            match api::post::<()>("/auth/reset", &request).await {
                Ok(()) => done.set(true),
                Err(err) => error.set(err.message),
            }
            busy.set(false);
        });
    };
    let to_sign_in = move |_| {
        app.go("/admin/overview");
        app.restart();
    };

    let changed = move || {
        view! {
            <div class="onb">
                <div class="hrow"><Brand text="Hearth"/></div>
                <h1>"Your password is changed"</h1>
                <p class="muted">"Every device was signed out. To play, open Bonfire and sign in with the new password."</p>
                <p class="muted">"If you run this Hearth, sign in to its admin pages here."</p>
                <div class="hrow" style="justify-content:flex-end"><button class="btn" on:click=to_sign_in>"Go to sign-in"</button></div>
            </div>
        }
    };
    let broken = move |message: String| {
        view! {
            <div class="onb">
                <div class="hrow"><Brand text="Hearth"/></div>
                <h1>"This link does not work"</h1>
                <p class="muted">{message}</p>
            </div>
        }
    };
    let form = move |username: String| {
        view! {
            <form class="onb" on:submit=submit.clone()>
                <div class="hrow"><Brand text="Hearth"/></div>
                <h1>{format!("Set a new password for {username}")}</h1>
                <p class="muted">"Setting it signs you out on every device, so you sign in again with the new password."</p>
                <Field label="New password" target="rs-pass">
                    <TextInput value=pass id="rs-pass" placeholder="At least 8 characters" kind="password" autocomplete="new-password"/>
                </Field>
                <Field label="New password again" target="rs-pass2">
                    <TextInput value=pass2 id="rs-pass2" label="New password again" kind="password" autocomplete="new-password"/>
                </Field>
                <ErrorNote message=error/>
                <div class="hrow" style="justify-content:flex-end;margin-top:4px">
                    <button class="btn pri" disabled=move || busy.get()>"Set password"</button>
                </div>
            </form>
        }
    };

    view! {
        <div class="lock">
            {move || match link.get() {
                _ if done.get() => changed().into_any(),
                Some(Ok(username)) => form(username).into_any(),
                Some(Err(message)) => broken(message).into_any(),
                None => view! { <Brand text="Hearth"/> }.into_any(),
            }}
        </div>
    }
}
