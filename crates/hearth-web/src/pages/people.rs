//! Who has an account on this Hearth, how new people get in, and where everyone is signed in.

use hearth_api::{
    Id,
    auth::{CreateInvite, Invite, Role, Session},
    people::{AdminUser, ResetLink, UserPatch},
};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    App, api,
    ui::{Avatar, Choice, Confirm, Icon, copy, delete_then, load_list, toast, toast_err, when},
};

/// The start of a link that is handed to someone else. An admin working on the Hearth's own
/// machine sees it as localhost, which is no use to anyone else, so the Hearth's address goes in.
async fn public_origin() -> String {
    let location = window().location();
    let (origin, host) = (location.origin().unwrap_or_default(), location.hostname().unwrap_or_default());
    if !matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]") {
        return origin;
    }
    match api::get::<hearth_api::settings::SettingsView>("/admin/settings").await {
        Ok(view) if !view.settings.address.is_empty() => origin.replacen(&host, &view.settings.address, 1),
        _ => origin,
    }
}

const ROLES: [(Role, &str); 2] = [(Role::Player, "Player"), (Role::Admin, "Admin")];
const USES: [(Option<u32>, &str); 3] = [(Some(1), "1 use"), (Some(5), "5 uses"), (None, "Unlimited")];

#[component]
pub fn People() -> impl IntoView {
    let app = expect_context::<App>();
    let users = RwSignal::new(Vec::<AdminUser>::new());
    let links = RwSignal::new(Vec::<ResetLink>::new());
    let invites = RwSignal::new(Vec::<Invite>::new());
    let sessions = RwSignal::new(Vec::<Session>::new());
    // The Hearth tells a link's address once, when the link is made. These are the ones made
    // on this visit: the link's id and its full address.
    let addresses = RwSignal::new(Vec::<(Id, String)>::new());
    let uses = RwSignal::new(Some(1u32));
    let removing = RwSignal::new(None::<AdminUser>);
    let me = app.me.get_untracked().map(|me| me.user.id);

    let load_users = move || load_list("/admin/users", users, "The accounts could not be loaded");
    let load_links = move || load_list("/admin/reset-links", links, "The reset links could not be loaded");
    let load_invites = move || load_list("/admin/invites", invites, "The invites could not be loaded");
    let load_sessions = move || load_list("/admin/sessions", sessions, "The signed-in devices could not be loaded");
    load_users();
    load_links();
    load_invites();
    load_sessions();

    let set_role = move |id: Id, role: Role| {
        spawn_local(async move {
            if let Err(err) = api::patch::<()>(&format!("/admin/users/{id}"), &UserPatch { role }).await {
                toast_err("The role was not changed", &err);
                // Reading the list again puts the select back.
                load_users();
            }
        });
    };
    let reset = move |id: Id| {
        spawn_local(async move {
            match api::post::<ResetLink>(&format!("/admin/users/{id}/reset-link"), &()).await {
                Ok(link) => {
                    if let Some(path) = &link.path {
                        let origin = public_origin().await;
                        addresses.update(|addresses| addresses.push((link.id, format!("{origin}{path}"))));
                    }
                    toast("ok", "Reset link created", format!("Copy it from the list and send it to {} yourself.", link.username));
                    load_links();
                }
                Err(err) => toast_err("No reset link was created", &err),
            }
        });
    };
    let create_invite = move |_| {
        let request = CreateInvite { max_uses: uses.get_untracked(), expires_in_days: None };
        spawn_local(async move {
            match api::post::<Invite>("/admin/invites", &request).await {
                Ok(_) => load_invites(),
                Err(err) => toast_err("No invite was created", &err),
            }
        });
    };

    let user_rows = move || {
        users
            .get()
            .into_iter()
            .map(|user| {
                let id = user.id;
                let own = me == Some(id);
                let role = RwSignal::new(user.role);
                let seen = if user.online {
                    view! { <span class="hrow" style="gap:6px;flex-wrap:nowrap"><i class="dot ok"></i>"Now"</span> }.into_any()
                } else {
                    user.last_seen_at.map_or("Never".to_string(), when).into_any()
                };
                let hours = user.playtime_seconds / 3600;
                let (devices, locked) = (user.devices, user.locked_until.is_some());
                let name = user.username.clone();
                view! {
                    <tr>
                        <td>
                            <span class="hrow" style="flex-wrap:nowrap">
                                <Avatar name=name.clone() size=24/>
                                <b style="font-weight:500">{name.clone()}</b>
                                {locked.then(|| view! { <span class="badge warn">"Locked"</span> })}
                            </span>
                        </td>
                        <td>
                            <Choice
                                value=role
                                options=ROLES.to_vec()
                                label=format!("Role for {name}")
                                style="height:26px"
                                disabled=own
                                on_commit=Callback::new(move |_: ()| set_role(id, role.get_untracked()))
                            />
                        </td>
                        <td class="muted">{seen}</td>
                        <td class="num">{hours}</td>
                        <td class="num">{devices}</td>
                        <td class="act">
                            <span class="hrow" style="flex-wrap:nowrap;justify-content:flex-end">
                                <button class="btn sm" on:click=move |_| reset(id)>"Reset password"</button>
                                {(!own).then(|| view! { <button class="btn sm danger" on:click=move |_| removing.set(Some(user.clone()))>"Remove"</button> })}
                            </span>
                        </td>
                    </tr>
                }
            })
            .collect_view()
    };

    let link_section = move || {
        let links = links.get();
        (!links.is_empty()).then(|| {
            let entries = links.into_iter().map(|link| {
                let id = link.id;
                let about = format!("for {}, expires {}", link.username, when(link.expires_at));
                let address = addresses.with(|known| known.iter().find(|(known, _)| *known == id).map(|(_, address)| address.clone()));
                let plain = address.is_none();
                let (text, note) = match address {
                    Some(address) => (address, about),
                    None => (format!("Link {about}"), String::new()),
                };
                let delete = Callback::new(move |_| delete_then(format!("/admin/reset-links/{id}"), "The link was not deleted", load_links));
                view! { <Entry text=text note=note plain=plain delete="Delete link" on_delete=delete/> }
            });
            view! {
                <section class="sec">
                    <h2 class="sh">"Password reset links"</h2>
                    <p class="muted" style="font-size:12px;max-width:72ch">
                        "Single use, valid for 24 hours. You send the link yourself. Using it signs that person out of their other devices."
                    </p>
                    {entries.collect_view()}
                </section>
            }
        })
    };

    let invite_entries = move || {
        invites
            .get()
            .into_iter()
            .map(|invite| {
                let id = invite.id;
                let used = match invite.max_uses {
                    Some(max) => format!("{} of {max} used", invite.uses),
                    None => format!("{} used", invite.uses),
                };
                let note = match invite.expires_at {
                    Some(at) => format!("{used} · expires {}", when(at)),
                    None => used,
                };
                let delete = Callback::new(move |_| delete_then(format!("/admin/invites/{id}"), "The invite was not deleted", load_invites));
                view! { <Entry text=invite.code note=note delete="Delete invite" on_delete=delete/> }
            })
            .collect_view()
    };

    let session_rows = move || {
        sessions
            .get()
            .into_iter()
            .map(|session| {
                let id = session.id;
                let seen = format!("last seen {}", when(session.last_seen_at));
                let detail = [session.platform, session.client, seen].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ");
                let sign_out = move |_| {
                    delete_then(format!("/auth/sessions/{id}"), "Not signed out", move || {
                        load_sessions();
                        load_users();
                    })
                };
                view! {
                    <div class="row" style="min-height:46px">
                        <div class="col">
                            <span class="t">{format!("{} · {}", session.username, session.device_name)}</span>
                            <span class="d">{detail}</span>
                        </div>
                        {if session.current {
                            view! { <span class="badge">"This browser"</span> }.into_any()
                        } else {
                            view! { <button class="btn sm" on:click=sign_out>"Sign out"</button> }.into_any()
                        }}
                    </div>
                }
            })
            .collect_view()
    };

    let remove_dialog = move || {
        removing.get().map(|user| {
            let id = user.id;
            let confirm = Callback::new(move |_| {
                removing.set(None);
                delete_then(format!("/admin/users/{id}"), "Not removed", move || {
                    toast("ok", "Removed", "Their saves are kept for 30 days.");
                    load_users();
                    load_links();
                    load_sessions();
                });
            });
            view! {
                <Confirm
                    title=format!("Remove {}", user.username)
                    text=format!("{} is signed out everywhere and can no longer sign in. Their saves are kept for 30 days.", user.username)
                    action="Remove"
                    on_confirm=confirm
                    on_close=Callback::new(move |_| removing.set(None))
                />
            }
        })
    };

    view! {
        <div class="view">
            <div class="ph">
                <div>
                    <span class="lab">
                        {move || {
                            let count = users.with(Vec::len);
                            format!("{count} {} on {}", if count == 1 { "account" } else { "accounts" }, app.name())
                        }}
                    </span>
                    <h1 class="pt">"People and invites"</h1>
                </div>
            </div>
            <div class="card tblw">
                <table class="tbl">
                    <thead>
                        <tr>
                            <th>"Name"</th>
                            <th>"Role"</th>
                            <th>"Last seen"</th>
                            <th class="num">"Hours on this Hearth"</th>
                            <th class="num">"PCs"</th>
                            <th></th>
                        </tr>
                    </thead>
                    <tbody>{user_rows}</tbody>
                </table>
            </div>
            {link_section}
            <div class="cols even">
                <section class="sec">
                    <h2 class="sh">"Invites"</h2>
                    <div class="card pad col" style="gap:10px">
                        <div class="hrow">
                            <Choice value=uses options=USES.to_vec() label="Uses"/>
                            <button class="btn pri" on:click=create_invite>"Create invite"</button>
                        </div>
                        {invite_entries}
                        <span class="faint" style="font-size:12px">
                            "People enter the code when they add this Hearth, then pick their own username and password."
                        </span>
                    </div>
                </section>
                <section class="sec">
                    <h2 class="sh">"Signed-in devices"</h2>
                    <div class="card rows">{session_rows}</div>
                </section>
            </div>
        </div>
        {remove_dialog}
    }
}

/// A line in a code box: a value to hand to someone, what else is known about it, and a way
/// to delete it. `plain` is for a line that only describes, with nothing to copy.
#[component]
fn Entry(
    text: String,
    #[prop(optional)] note: String,
    #[prop(optional)] plain: bool,
    delete: &'static str,
    on_delete: Callback<()>,
) -> impl IntoView {
    let copied = text.clone();
    view! {
        <div class="code">
            <span style={ if plain { "font:400 13px var(--sans);color:var(--fg-2)" } else { "" } }>{text}</span>
            {(!note.is_empty()).then(|| view! { <span class="faint" style="flex:none;font:400 12px var(--sans)">{note}</span> })}
            {(!plain).then(|| view! { <button type="button" class="btn sm" on:click=move |_| copy(&copied)>"Copy"</button> })}
            <button type="button" class="ib" aria-label=delete on:click=move |_| on_delete.run(())><Icon name="trash" size=14/></button>
        </div>
    }
}
