//! Every option the Hearth has. A change is saved the moment it is made.

use hearth_api::{
    admin::Maintenance,
    auth::{TotpCode, TotpSetup},
    library::MetadataTest,
    settings::{MetadataProvider, SettingsView, TlsMode},
};
use leptos::{prelude::*, task::spawn_local};

use super::setup::Setup;
use crate::{
    App, api,
    folders::FolderEditor,
    form::{Form, JOIN_CHOICES, QUOTA_CHOICES, SIGNOUT_CHOICES, THRESHOLD_CHOICES, TLS_CHOICES, tls_note},
    ui::{Choice, Dialog, ErrorNote, Field, Group, Srow, TextInput, Trow, size, sleep, toast, toast_err, when},
};

#[component]
pub fn Settings() -> impl IntoView {
    let app = expect_context::<App>();
    let form = Form::new();
    let stored = RwSignal::new(None::<SettingsView>);
    let rerun = RwSignal::new(false);
    let totp_dialog = RwSignal::new(None::<TotpAction>);

    let load = move || {
        spawn_local(async move {
            match api::get::<SettingsView>("/admin/settings").await {
                Ok(view) => {
                    form.load(&view.settings);
                    stored.set(Some(view));
                }
                Err(err) => toast_err("The settings could not be loaded", &err),
            }
        });
    };
    load();

    // Saves the form, and answers whether the Hearth took it.
    let store = move || async move {
        let patch = match form.patch() {
            Ok(patch) => patch,
            Err(message) => {
                toast("warn", "Not saved", message);
                return false;
            }
        };
        let before = stored.get_untracked().map(|view| view.settings);
        match api::patch::<SettingsView>("/admin/settings", &patch).await {
            Ok(view) => {
                // Only the secrets are refreshed. Reloading everything would wipe a field
                // the admin is typing in while this answer was on its way.
                form.igdb_client_secret.set(String::new());
                form.igdb_client_secret_set.set(view.settings.igdb_client_secret_set);
                form.sgdb_api_key.set(String::new());
                form.sgdb_api_key_set.set(view.settings.sgdb_api_key_set);
                let now = view.settings.clone();
                stored.set(Some(view));
                app.refresh_info();
                if before.is_some_and(|before| (before.port, before.tls) != (now.port, now.tls)) {
                    follow(now.port, now.tls != TlsMode::Proxy).await;
                }
                true
            }
            Err(err) => {
                toast_err("Not saved", &err);
                load();
                false
            }
        }
    };
    let save = Callback::new(move |_: ()| {
        spawn_local(async move {
            store().await;
        });
    });
    // The Hearth tests the keys it has stored, so what is in the fields is saved first.
    let test = move |provider: MetadataProvider| {
        spawn_local(async move {
            if !store().await {
                return;
            }
            match api::post::<()>("/admin/metadata/test", &MetadataTest { provider }).await {
                Ok(()) => {
                    let name = if provider == MetadataProvider::Igdb { "IGDB" } else { "SteamGridDB" };
                    toast("ok", "The key works", format!("{name} answered."));
                }
                Err(err) => toast_err("The key does not work", &err),
            }
        });
    };

    // Requiring two-step sign-in starts with the admin who asks for it.
    let require_totp = Callback::new(move |_: ()| {
        let has_it = app.me.get_untracked().is_some_and(|me| me.totp_enabled);
        if form.admin_totp_required.get_untracked() && !has_it {
            form.admin_totp_required.set(false);
            totp_dialog.set(Some(TotpAction::SetUpAndRequire));
        } else {
            save.run(());
        }
    });
    let totp_done = Callback::new(move |action: TotpAction| {
        totp_dialog.set(None);
        app.restart();
        if action == TotpAction::SetUpAndRequire {
            form.admin_totp_required.set(true);
            save.run(());
        }
    });
    let has_totp = move || app.me.get().is_some_and(|me| me.totp_enabled);

    // Backups and updates.
    let maintenance = RwSignal::new(None::<Maintenance>);
    // A check for updates or a backup is on its way.
    let busy = RwSignal::new(false);
    let updating = RwSignal::new(false);
    spawn_local(async move {
        match api::get::<Maintenance>("/admin/maintenance").await {
            Ok(found) => maintenance.set(Some(found)),
            Err(err) => toast_err("Backups and updates could not be loaded", &err),
        }
    });
    // Both the check and the backup answer with the maintenance details as they are afterwards.
    let maintain = move |path: &'static str, failed: &'static str, done: fn(&Maintenance)| {
        busy.set(true);
        spawn_local(async move {
            match api::post::<Maintenance>(path, &()).await {
                Ok(found) => {
                    done(&found);
                    maintenance.set(Some(found));
                }
                Err(err) => toast_err(failed, &err),
            }
            busy.set(false);
        });
    };
    let check = move |_| maintain("/admin/update/check", "The check for updates did not work", |_| {});
    let back_up = move |_| {
        maintain("/admin/backup", "The backup did not work", |found| {
            if let Some(backup) = &found.backup {
                toast("ok", "Backup finished", format!("{} in {}", size(backup.bytes), found.backup_folder));
            }
        })
    };
    let update = move |_| {
        updating.set(true);
        spawn_local(async move {
            match api::post::<serde::de::IgnoredAny>("/admin/update", &()).await {
                Ok(_) => {
                    // The Hearth restarts now. After that the page looks for it again.
                    sleep(20_000).await;
                    app.restart();
                }
                Err(err) => {
                    updating.set(false);
                    toast_err("The update did not start", &err);
                }
            }
        });
    };
    // What the last look for a newer version found: its name, and whether it is newer.
    let newer = move || maintenance.with(|found| found.as_ref().and_then(|found| found.update.as_ref()).map(|update| (update.latest.clone(), update.available)));
    let version_text = move || {
        let version = stored.with(|view| view.as_ref().map(|view| view.version.clone())).unwrap_or_default();
        match newer() {
            Some((latest, true)) => {
                format!("hearth {version}. {latest} is out. Updating restarts the Hearth for about a minute, and downloads resume afterwards.")
            }
            _ => format!("hearth {version}"),
        }
    };
    let backup_text = move || {
        maintenance.with(|found| {
            found.as_ref().map_or(String::new(), |found| match &found.backup {
                Some(backup) => format!("Last {} · {} · {}", when(backup.at), size(backup.bytes), found.backup_folder),
                None => format!("No backup yet · {}", found.backup_folder),
            })
        })
    };
    let secret_hint = |set: RwSignal<bool>, what: &'static str| move || if set.get() { "Stored. Type a new one to replace it.".to_string() } else { what.to_string() };

    view! {
        <div class="view" style="max-width:900px">
            <div class="ph">
                <div>
                    <h1 class="pt">"Settings"</h1>
                    <p class="sub" style="max-width:70ch">
                        "Everything about this Hearth is set here and kept in its database. There is no .env or config file to edit."
                    </p>
                </div>
            </div>

            <Group
                title="Address and joining"
                note="A new address or port takes effect at once: the Hearth starts listening there on its own, and downloads resume afterwards."
            >
                <Srow title="Name" desc="What people see in their list of Hearths.">
                    <TextInput value=form.name placeholder="home" style="width:150px;max-width:100%" on_commit=save/>
                </Srow>
                <Srow title="Address and port" desc="What people type when they add this Hearth.">
                    <TextInput value=form.address placeholder="home.lan" class="mono" style="width:160px;max-width:100%" on_commit=save/>
                    <TextInput value=form.port placeholder="7300" class="mono" style="width:72px;max-width:100%" on_commit=save/>
                </Srow>
                <Srow title="Secure connection" desc=Signal::derive(move || tls_note(form.tls.get()).to_string())>
                    <Choice value=form.tls options=TLS_CHOICES.to_vec() label="Secure connection" on_commit=save/>
                </Srow>
                <Srow title="Who can join">
                    <Choice value=form.join options=JOIN_CHOICES.to_vec() label="Who can join" on_commit=save/>
                </Srow>
            </Group>

            <section class="sec">
                <h2 class="sh">"Folders"</h2>
                <p class="muted" style="font-size:12px;max-width:72ch">
                    "The Hearth only reads the games folders and never changes your files. Add as many as you like, for example one per disk."
                </p>
                <FolderEditor
                    form=form
                    endpoint="/admin/folders"
                    data_folder=Signal::derive(move || stored.get().map(|view| view.data_folder).unwrap_or_default())
                    on_commit=save
                />
            </section>

            <Group title="Library and metadata">
                <Trow title="Watch for changes" desc="New files in a games folder are picked up on their own." checked=form.watch_folders on_commit=save/>
                <Srow title="Match files on their own" desc="Files that score below this wait in Metadata review.">
                    <Choice value=form.match_threshold options=THRESHOLD_CHOICES.to_vec() label="Match threshold" on_commit=save/>
                </Srow>
                <Trow title="IGDB" desc="Descriptions, years and genres." checked=form.igdb_enabled on_commit=save/>
                <Srow title="IGDB key" desc=Signal::derive(secret_hint(form.igdb_client_secret_set, "From a free Twitch developer account."))>
                    <TextInput value=form.igdb_client_id placeholder="Client ID" class="mono" style="width:150px;max-width:100%" on_commit=save/>
                    <TextInput
                        value=form.igdb_client_secret
                        placeholder="Client secret"
                        class="mono"
                        kind="password"
                        autocomplete="new-password"
                        style="width:150px;max-width:100%"
                        on_commit=save
                    />
                    <button class="btn sm" on:click=move |_| test(MetadataProvider::Igdb)>"Test"</button>
                </Srow>
                <Trow title="SteamGridDB" desc="Covers, heroes and logos." checked=form.sgdb_enabled on_commit=save/>
                <Srow title="SteamGridDB key" desc=Signal::derive(secret_hint(form.sgdb_api_key_set, "From your SteamGridDB profile."))>
                    <TextInput
                        value=form.sgdb_api_key
                        placeholder="API key"
                        class="mono"
                        kind="password"
                        autocomplete="new-password"
                        style="width:220px;max-width:100%"
                        on_commit=save
                    />
                    <button class="btn sm" on:click=move |_| test(MetadataProvider::Sgdb)>"Test"</button>
                </Srow>
            </Group>

            <Group title="Playing together">
                <Trow title="Bonfire relay" desc="Carries lobbies when a direct connection fails." checked=form.relay_enabled on_commit=save/>
                <Srow title="Relay port" desc="UDP. Forward it on your router if friends join from outside your network.">
                    <TextInput value=form.relay_port placeholder="7301" class="mono" style="width:72px;max-width:100%" on_commit=save/>
                </Srow>
                <Trow
                    title="Bonfire networking for new games"
                    desc="Lobbies, invites and achievements run through this Hearth instead of Steam."
                    checked=form.networking_default
                    on_commit=save
                />
                <Srow title="Rest storage per person" desc="Space for cloud saves.">
                    <Choice value=form.save_quota_gb options=QUOTA_CHOICES.to_vec() label="Rest storage per person" on_commit=save/>
                </Srow>
            </Group>

            <Group title="Sign-in and security">
                <Trow
                    title="Two-step sign-in for admins"
                    desc="A 6-digit code from an authenticator app."
                    checked=form.admin_totp_required
                    on_commit=require_totp
                />
                <Srow
                    title="Your authenticator app"
                    desc=Signal::derive(move || {
                        if has_totp() { "Two-step sign-in is on for your account." } else { "Two-step sign-in is off for your account." }.to_string()
                    })
                >
                    {move || {
                        if has_totp() {
                            view! { <button class="btn sm" on:click=move |_| totp_dialog.set(Some(TotpAction::Remove))>"Remove"</button> }.into_any()
                        } else {
                            view! { <button class="btn sm" on:click=move |_| totp_dialog.set(Some(TotpAction::SetUp))>"Set up"</button> }.into_any()
                        }
                    }}
                </Srow>
                <Srow title="Sign people out after">
                    <Choice value=form.session_days options=SIGNOUT_CHOICES.to_vec() label="Sign people out after" on_commit=save/>
                </Srow>
                <Trow
                    title="Lock an account after 5 wrong passwords"
                    desc="Unlocks after 15 minutes, or when an admin sends a reset link."
                    checked=form.lockout_enabled
                    on_commit=save
                />
            </Group>

            <Group title="Maintenance">
                <Srow title="Version" desc=Signal::derive(version_text)>
                    {move || {
                        newer()
                            .map(|(latest, available)| {
                                if available {
                                    view! {
                                        <button class="btn sm" disabled=move || updating.get() on:click=update>
                                            {move || if updating.get() { "Updating".to_string() } else { format!("Update to {latest}") }}
                                        </button>
                                    }
                                        .into_any()
                                } else {
                                    view! { <span class="badge ok">"Up to date"</span> }.into_any()
                                }
                            })
                    }}
                    <button class="btn sm" disabled=move || busy.get() || updating.get() on:click=check>"Check for updates"</button>
                </Srow>
                <Trow title="Back up the database every night" desc="Accounts, playtime, collections and news." checked=form.backup_nightly on_commit=save/>
                <Srow title="Database backup" desc=Signal::derive(backup_text)>
                    <Show when=move || form.backup_nightly.get()>
                        <TextInput
                            value=form.backup_time
                            placeholder="04:00"
                            label="Time of the nightly backup, in the Hearth's local time"
                            class="mono"
                            style="width:72px;max-width:100%"
                            on_commit=save
                        />
                    </Show>
                    <button class="btn sm" disabled=move || busy.get() on:click=back_up>"Back up now"</button>
                </Srow>
                <Srow title="First-time setup" desc="Walks through the address, folders and extras again.">
                    <button class="btn sm" on:click=move |_| rerun.set(true)>"Run setup again"</button>
                </Srow>
            </Group>
        </div>

        <Show when=move || rerun.get()>
            <Setup
                rerun=true
                on_close=Callback::new(move |_| {
                    rerun.set(false);
                    load();
                })
            />
        </Show>
        {move || totp_dialog.get().map(|action| view! { <TotpDialog action=action on_done=totp_done on_close=Callback::new(move |_| totp_dialog.set(None))/> })}
    }
}

/// After a change of port or encryption the Hearth listens somewhere else. The page goes along.
async fn follow(port: u16, https: bool) {
    let location = window().location();
    let host = location.hostname().unwrap_or_else(|_| "localhost".into());
    let url = format!("{}://{host}:{port}/admin/settings", if https { "https" } else { "http" });
    toast("ok", "The Hearth moved", format!("It now answers at {url}. This page follows in a moment."));
    sleep(1800).await;
    let _ = location.set_href(&url);
}

#[derive(Clone, Copy, PartialEq)]
pub enum TotpAction {
    SetUp,
    /// Set it up, then require it for every admin.
    SetUpAndRequire,
    Remove,
}

/// Sets up or removes two-step sign-in for the admin who is signed in.
#[component]
fn TotpDialog(action: TotpAction, on_done: Callback<TotpAction>, on_close: Callback<()>) -> impl IntoView {
    let removing = action == TotpAction::Remove;
    let setup = RwSignal::new(None::<TotpSetup>);
    let code = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    if !removing {
        spawn_local(async move {
            match api::post::<TotpSetup>("/auth/totp/setup", &()).await {
                Ok(found) => setup.set(Some(found)),
                Err(err) => error.set(err.message),
            }
        });
    }
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let request = TotpCode { code: code.get_untracked() };
        spawn_local(async move {
            let path = if removing { "/auth/totp/disable" } else { "/auth/totp/enable" };
            match api::post::<()>(path, &request).await {
                Ok(()) => {
                    toast("ok", if removing { "Two-step sign-in is off" } else { "Two-step sign-in is on" }, "");
                    on_done.run(action);
                }
                Err(err) => error.set(err.message),
            }
        });
    };
    let qr = move || {
        setup.get().and_then(|setup| {
            let code = qrcode::QrCode::new(setup.uri.as_bytes()).ok()?;
            let svg = code.render::<qrcode::render::svg::Color>().min_dimensions(176, 176).quiet_zone(true).build();
            Some(view! {
                <div class="hrow" style="align-items:flex-start;gap:14px">
                    <div style="background:#fff;border-radius:8px;padding:4px;line-height:0;flex:none" inner_html=svg></div>
                    <div class="col" style="flex:1 1 180px">
                        <span class="muted">"Scan this with an authenticator app, or type the secret in by hand."</span>
                        <div class="code"><span>{setup.secret}</span></div>
                    </div>
                </div>
            })
        })
    };

    view! {
        <Dialog title={ if removing { "Remove two-step sign-in" } else { "Set up two-step sign-in" } } on_close=on_close>
            <form on:submit=submit style="display:contents">
                <div class="db">
                    {qr}
                    <Field label={ if removing { "Code from your authenticator app, to confirm" } else { "Then enter the 6-digit code the app shows" } } target="totp-code">
                        <TextInput value=code id="totp-code" class="mono" placeholder="6 digits" autocomplete="one-time-code"/>
                    </Field>
                    <ErrorNote message=error/>
                </div>
                <footer>
                    <button type="button" class="btn" on:click=move |_| on_close.run(())>"Cancel"</button>
                    <button class="btn pri">{if removing { "Remove" } else { "Switch on" }}</button>
                </footer>
            </form>
        </Dialog>
    }
}
