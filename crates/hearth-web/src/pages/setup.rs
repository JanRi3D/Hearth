//! First-time setup: admin, address, folders, extras, finish. The same five steps can be
//! walked again later from Settings, where they save through the admin API instead.

use hearth_api::{
    settings::{SettingsView, TlsMode},
    setup::{SetupDefaults, SetupDone, SetupRequest},
};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    App, api,
    folders::FolderEditor,
    form::{Form, JOIN_CHOICES, QUOTA_CHOICES, TLS_CHOICES, tls_note},
    ui::{Brand, Choice, CodeBox, ErrorNote, Field, Group, Icon, Srow, TextInput, Trow, noop, sleep, toast},
};

const STEPS: [&str; 5] = ["Admin", "Address", "Folders", "Extras", "Finish"];
const STARTING: [&str; 5] = [
    "Creating the admin account",
    "Saving the settings to the database",
    "Reading the games folders",
    "Matching covers and details",
    "Starting the relay",
];

#[component]
pub fn Setup(
    /// Walked again by a signed-in admin: no setup code and no new account.
    #[prop(optional)]
    rerun: bool,
    #[prop(default = noop(), into)] on_close: Callback<()>,
) -> impl IntoView {
    let app = expect_context::<App>();
    let form = Form::new();
    let step = RwSignal::new(if rerun { 2u8 } else { 1 });
    let error = RwSignal::new(String::new());
    let (code, user, pass, pass2) =
        (RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()));
    let data_folder = RwSignal::new(String::new());
    let loaded = RwSignal::new(false);
    let busy = RwSignal::new(0usize);
    let done = RwSignal::new(None::<SetupDone>);
    let endpoint = if rerun { "/admin/folders" } else { "/setup" };

    if rerun {
        spawn_local(async move {
            match api::get::<SettingsView>("/admin/settings").await {
                Ok(view) => {
                    form.load(&view.settings);
                    data_folder.set(view.data_folder);
                }
                Err(err) => error.set(err.message),
            }
        });
    }

    let advance = move || {
        error.set(String::new());
        let fail = |message: &str| error.set(message.to_string());
        match step.get_untracked() {
            1 => {
                let typed = code.get_untracked();
                if typed.chars().filter(char::is_ascii_alphanumeric).count() != 8 {
                    return fail("The setup code looks like 7Q2K-XM4D. It is printed where the Hearth was started.");
                }
                if user.get_untracked().trim().is_empty() {
                    return fail("Choose a username for the admin.");
                }
                if pass.get_untracked().chars().count() < 8 {
                    return fail("Use at least 8 characters for the password.");
                }
                if pass.get_untracked() != pass2.get_untracked() {
                    return fail("The two passwords are not the same.");
                }
                api::set_setup_code(&typed);
                spawn_local(async move {
                    match api::post::<SetupDefaults>("/setup/begin", &()).await {
                        Ok(defaults) => {
                            // Going back to this step must not wipe what was entered on the later ones.
                            if !loaded.get_untracked() {
                                form.load(&defaults.settings);
                                data_folder.set(defaults.data_folder);
                                loaded.set(true);
                            }
                            step.set(2);
                        }
                        Err(err) => error.set(err.message),
                    }
                });
            }
            2 => {
                let address = form.address.get_untracked();
                if form.name.get_untracked().trim().is_empty() {
                    return fail("Give the Hearth a name.");
                }
                if address.trim().is_empty() || !address.trim().chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':')) {
                    return fail("The address is a name or IP without spaces, for example home.lan.");
                }
                if !form.port.get_untracked().trim().parse::<u16>().is_ok_and(|port| port > 0) {
                    return fail("The port is a number between 1 and 65535.");
                }
                step.set(3);
            }
            3 => {
                if form.games_folders.with_untracked(Vec::is_empty) {
                    return fail("Add at least one games folder.");
                }
                step.set(4);
            }
            4 => {
                let (id, secret) = (form.igdb_client_id.get_untracked(), form.igdb_client_secret.get_untracked());
                let has_secret = !secret.trim().is_empty() || form.igdb_client_secret_set.get_untracked();
                if form.igdb_enabled.get_untracked() && id.trim().is_empty() != !has_secret {
                    return fail("IGDB needs both the client ID and the secret. Leave both empty to add them later.");
                }
                step.set(5);
            }
            _ if rerun => {
                let patch = match form.patch() {
                    Ok(patch) => patch,
                    Err(message) => return fail(&message),
                };
                spawn_local(async move {
                    match api::patch::<SettingsView>("/admin/settings", &patch).await {
                        Ok(_) => {
                            toast("ok", "Settings saved", "");
                            on_close.run(());
                        }
                        Err(err) => error.set(err.message),
                    }
                });
            }
            _ => {
                let settings = match form.settings() {
                    Ok(settings) => settings,
                    Err(message) => return fail(&message),
                };
                let request = SetupRequest { username: user.get_untracked().trim().to_string(), password: pass.get_untracked(), settings };
                busy.set(1);
                spawn_local(async move {
                    match api::post::<SetupDone>("/setup/complete", &request).await {
                        Ok(finished) => {
                            api::set_setup_code("");
                            for line in 2..=STARTING.len() {
                                busy.set(line);
                                sleep(350).await;
                            }
                            busy.set(0);
                            done.set(Some(finished));
                        }
                        Err(err) => {
                            busy.set(0);
                            error.set(err.message);
                        }
                    }
                });
            }
        }
    };

    let back = move |_| {
        error.set(String::new());
        match step.get_untracked() {
            2 if rerun => on_close.run(()),
            current if current > 1 => step.set(current - 1),
            _ => {}
        }
    };

    let top = move || {
        view! {
            <div class="hrow">
                <Brand text="Hearth setup"/>
                <span class="sp"></span>
                <Show when=move || done.get().is_none() && busy.get() == 0>
                    <span class="lab">{move || format!("Step {} of 5", step.get())}</span>
                </Show>
            </div>
        }
    };

    // ---------- the five steps ----------

    let step_admin = move || {
        view! {
            <h1>"Create the admin account"</h1>
            <p class="muted">"This Hearth has no owner yet. The setup code proves you are the person who started it."</p>
            <Field label="Setup code" target="su-code"><TextInput value=code id="su-code" placeholder="7Q2K-XM4D" class="mono"/></Field>
            <p class="faint" style="font-size:12px;margin-top:-8px">"Printed in the window where the Hearth was started."</p>
            <div class="formgrid">
                <Field label="Username" target="su-user"><TextInput value=user id="su-user" placeholder="mika" autocomplete="username"/></Field>
                <Field label="Password" target="su-pass">
                    <TextInput value=pass id="su-pass" placeholder="At least 8 characters" kind="password" autocomplete="new-password"/>
                </Field>
                <Field label="Password again" target="su-pass2">
                    <TextInput value=pass2 id="su-pass2" label="Password again" kind="password" autocomplete="new-password"/>
                </Field>
            </div>
        }
    };

    let step_address = move || {
        view! {
            <h1>"Name and address"</h1>
            <p class="muted">"How friends find this Hearth. All of it can be changed later under Settings."</p>
            <div class="formgrid">
                <Field label="Name" target="hs-name"><TextInput value=form.name id="hs-name" placeholder="home"/></Field>
                <Field label="Address" target="hs-addr"><TextInput value=form.address id="hs-addr" placeholder="home.lan" class="mono"/></Field>
                <Field label="Port" target="hs-port"><TextInput value=form.port id="hs-port" placeholder="7300" class="mono"/></Field>
            </div>
            <div class="formgrid">
                <Field label="Secure connection" target="hs-https">
                    <Choice value=form.tls options=TLS_CHOICES.to_vec() label="Secure connection" id="hs-https"/>
                </Field>
                <Field label="Who can join" target="hs-join">
                    <Choice value=form.join options=JOIN_CHOICES.to_vec() label="Who can join" id="hs-join"/>
                </Field>
            </div>
            <div class="note"><Icon name="info" size=14/><span>{move || tls_note(form.tls.get())}</span></div>
        }
    };

    let step_folders = move || {
        view! {
            <h1>"Where are your games?"</h1>
            <p class="muted">
                "Point the Hearth at the folders that hold your games. It reads them, finds covers and details, and never changes your files."
            </p>
            <FolderEditor form=form endpoint=endpoint data_folder=data_folder/>
        }
    };

    let step_extras = move || {
        view! {
            <h1>"Extras"</h1>
            <p class="muted">"All optional. Skip anything now and switch it on later under Settings."</p>
            <Group title="Covers and details" note="Without keys the Hearth still lists every game, with a plain cover and the name of its folder.">
                <Trow title="IGDB" desc="Descriptions, years and genres. Needs a free key." checked=form.igdb_enabled/>
                <Show when=move || form.igdb_enabled.get()>
                    <Srow title="IGDB key" desc="From a free Twitch developer account.">
                        <TextInput value=form.igdb_client_id placeholder="Client ID" class="mono" style="width:140px;max-width:100%"/>
                        <TextInput value=form.igdb_client_secret placeholder="Client secret" class="mono" kind="password" autocomplete="new-password" style="width:140px;max-width:100%"/>
                    </Srow>
                </Show>
                <Trow title="SteamGridDB" desc="Covers, heroes and logos. Needs a free key." checked=form.sgdb_enabled/>
                <Show when=move || form.sgdb_enabled.get()>
                    <Srow title="SteamGridDB key" desc="From your SteamGridDB profile.">
                        <TextInput value=form.sgdb_api_key placeholder="API key" class="mono" kind="password" autocomplete="new-password" style="width:220px;max-width:100%"/>
                    </Srow>
                </Show>
            </Group>
            <Group title="Playing together">
                <Trow title="Bonfire relay" desc="Carries lobbies when a direct connection fails." checked=form.relay_enabled/>
                <Trow
                    title="Bonfire networking for new games"
                    desc="Lobbies, invites and achievements run through this Hearth instead of Steam."
                    checked=form.networking_default
                />
            </Group>
            <Group title="Saves and backups">
                <Srow title="Rest storage per person" desc="Space for cloud saves.">
                    <Choice value=form.save_quota_gb options=QUOTA_CHOICES.to_vec() label="Rest storage per person"/>
                </Srow>
                <Trow title="Back up the database every night" desc="Accounts, playtime, collections and news." checked=form.backup_nightly/>
            </Group>
        }
    };

    let step_finish = move || {
        let admin = if rerun { app.me.get_untracked().map(|me| me.user.username).unwrap_or_default() } else { user.get_untracked().trim().to_string() };
        let scheme = if form.tls.get_untracked() == TlsMode::Proxy { "http" } else { "https" };
        let join = JOIN_CHOICES.iter().find(|(mode, _)| *mode == form.join.get_untracked()).map(|(_, text)| text.to_lowercase()).unwrap_or_default();
        let key_state = |name: &str, on: bool, has_key: bool| on.then(|| if has_key { name.to_string() } else { format!("{name}, key missing") });
        let covers = [
            key_state(
                "IGDB",
                form.igdb_enabled.get_untracked(),
                !form.igdb_client_id.get_untracked().trim().is_empty(),
            ),
            key_state(
                "SteamGridDB",
                form.sgdb_enabled.get_untracked(),
                !form.sgdb_api_key.get_untracked().trim().is_empty() || form.sgdb_api_key_set.get_untracked(),
            ),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let on_off = |on: bool| if on { "on" } else { "off" };
        let rows = [
            ("Admin", admin),
            ("Address", format!("{scheme}://{}:{} · {join}", form.address.get_untracked().trim(), form.port.get_untracked().trim())),
            ("Games folders", form.games_folders.get_untracked().join("\n")),
            ("Emulators folder", form.emulators_folder.get_untracked()),
            ("Covers and details", if covers.is_empty() { "Off".into() } else { covers }),
            (
                "Playing together",
                format!(
                    "Relay {} · networking {}",
                    if form.relay_enabled.get_untracked() { format!("on, UDP {}", form.relay_port.get_untracked()) } else { "off".into() },
                    on_off(form.networking_default.get_untracked())
                ),
            ),
            (
                "Saves and backups",
                format!("{} GB per person · nightly backup {}", form.save_quota_gb.get_untracked(), on_off(form.backup_nightly.get_untracked())),
            ),
            ("Data folder", data_folder.get_untracked()),
        ];
        view! {
            <h1>{if rerun { "Check it over" } else { "Ready to start" }}</h1>
            <p class="muted">
                "Check it over. Everything here is saved in the Hearth's database, so there is no .env or config file to keep in step."
            </p>
            <div class="card rows">
                {rows
                    .into_iter()
                    .map(|(label, value)| {
                        view! {
                            <div class="row" style="align-items:flex-start;min-height:40px">
                                <span class="muted" style="width:130px;flex:none">{label}</span>
                                <span class="mono" style="font-size:12.5px;overflow-wrap:anywhere;min-width:0;white-space:pre-line">{value}</span>
                            </div>
                        }
                    })
                    .collect_view()}
            </div>
        }
    };

    // ---------- starting, and done ----------

    let starting = move || {
        view! {
            <div class="onb wide">
                {top}
                <h1>{move || format!("Starting {}", form.name.get())}</h1>
                <div class="card rows">
                    {STARTING
                        .iter()
                        .enumerate()
                        .map(|(index, line)| {
                            let line_no = index + 1;
                            view! {
                                <div class="row" style="min-height:44px">
                                    <span class="tico" class:ok=move || { line_no < busy.get() }>
                                        {move || {
                                            let at = busy.get();
                                            if line_no < at {
                                                view! { <Icon name="check" size=14 stroke=2.0/> }.into_any()
                                            } else if line_no == at {
                                                view! { <span class="spin" style="display:inline-flex"><Icon name="refresh" size=14 stroke=2.0/></span> }.into_any()
                                            } else {
                                                view! { <Icon name="clock" size=14/> }.into_any()
                                            }
                                        }}
                                    </span>
                                    <span class:faint=move || { line_no > busy.get() }>{*line}</span>
                                </div>
                            }
                        })
                        .collect_view()}
                </div>
            </div>
        }
    };

    let finished = move |result: SetupDone| {
        let address = result.address.clone();
        let invite = result.invite_code.clone();
        let https = result.url.starts_with("https://");
        let moved = result.moved;
        let open_admin = move |_| {
            if !result.moved {
                // Same address as before, and the Hearth has already signed this browser in.
                return app.restart();
            }
            // Keep the name this browser already reaches the Hearth by. Only scheme and port change.
            let location = window().location();
            let host = location.hostname().unwrap_or_else(|_| "localhost".into());
            let port = result.address.rsplit(':').next().unwrap_or("7300");
            let token = result.handoff.clone().unwrap_or_default();
            let scheme = if https { "https" } else { "http" };
            let _ = location.set_href(&format!("{scheme}://{host}:{port}/admin/overview#handoff={token}"));
        };
        view! {
            <div class="onb wide">
                {top}
                <h1>{move || format!("{} is running", form.name.get())}</h1>
                <p class="muted">
                    "Setup is finished and nothing is left to configure by hand. Give your friends the address and an invite code. They enter both in Bonfire when they add a Hearth."
                </p>
                <Field label="Address"><CodeBox text=address/></Field>
                <Field label="First invite · 5 uses · expires in 7 days"><CodeBox text=invite/></Field>
                {(moved && https)
                    .then(|| {
                        view! {
                            <div class="note">
                                <Icon name="info" size=14/>
                                <span>
                                    "The Hearth now answers over HTTPS with a certificate it made itself. Your browser warns about that once. Choose to continue, and the admin opens."
                                </span>
                            </div>
                        }
                    })}
                <div class="hrow" style="justify-content:flex-end;margin-top:4px">
                    <button type="button" class="btn pri" on:click=open_admin>"Open the admin"</button>
                </div>
            </div>
        }
    };

    let wizard = move || {
        view! {
            <form
                class="onb wide"
                on:submit=move |ev| {
                    ev.prevent_default();
                    advance();
                }
            >
                {top}
                <ol class="steps">
                    {STEPS
                        .iter()
                        .enumerate()
                        .map(|(index, name)| {
                            let number = index as u8 + 1;
                            view! { <li class:on=move || { step.get() == number } class:past=move || { step.get() > number }>{*name}</li> }
                        })
                        .collect_view()}
                </ol>
                {move || match step.get() {
                    1 => step_admin().into_any(),
                    2 => step_address().into_any(),
                    3 => step_folders().into_any(),
                    4 => step_extras().into_any(),
                    _ => step_finish().into_any(),
                }}
                <ErrorNote message=error/>
                <div class="hrow" style="justify-content:space-between;margin-top:4px">
                    <button type="button" class="btn ghost" on:click=back style=move || { if step.get() == 1 { "visibility:hidden" } else { "" } }>
                        {move || if rerun && step.get() == 2 { "Close" } else { "Back" }}
                    </button>
                    <button class="btn pri">
                        {move || match step.get() {
                            5 if rerun => "Save",
                            5 => "Start the Hearth",
                            _ => "Continue",
                        }}
                    </button>
                </div>
            </form>
        }
    };

    view! {
        <div class="lock">
            {move || match done.get() {
                Some(result) => finished(result).into_any(),
                None if busy.get() > 0 => starting().into_any(),
                None => wizard().into_any(),
            }}
        </div>
    }
}
