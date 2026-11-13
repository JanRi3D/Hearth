//! Firelink: the registry of dedicated game servers that players browse in Bonfire.

use hearth_api::{
    Id,
    community::{AddGameServer, GameServer, HEARTBEAT_SECONDS, Registry},
};
use leptos::{prelude::*, task::spawn_local};

use super::library::{load_titles, title_of};
use crate::{
    api,
    ui::{CodeBox, Confirm, ErrorNote, Field, Pick, TextInput, delete_then, every, picked, toast, toast_err},
};

#[component]
pub fn Firelink() -> impl IntoView {
    let data = RwSignal::new(None::<Registry>);
    let titles = RwSignal::new(Vec::<(Id, String)>::new());
    let game = RwSignal::new(None::<Id>);
    let (name, address) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let error = RwSignal::new(String::new());
    let renewing = RwSignal::new(false);

    // `quiet` is for the refresh on a timer, which does not nag while the Hearth is away.
    let load = move |quiet: bool| {
        spawn_local(async move {
            match api::get::<Registry>("/admin/game-servers").await {
                Ok(found) => data.set(Some(found)),
                Err(err) if !quiet => toast_err("The server registry could not be loaded", &err),
                Err(_) => {}
            }
        });
    };
    load(false);
    load_titles(titles);
    every(15, move || load(true));

    let add = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let Some(game_id) = picked(game, &titles.get_untracked()) else {
            return error.set("There is no game yet. Add one to the library first.".into());
        };
        let server = AddGameServer { game_id, name: name.get_untracked(), address: address.get_untracked() };
        spawn_local(async move {
            match api::post::<GameServer>("/admin/game-servers", &server).await {
                Ok(_) => {
                    name.set(String::new());
                    address.set(String::new());
                    error.set(String::new());
                    load(true);
                }
                Err(err) => error.set(err.message),
            }
        });
    };
    let renew = Callback::new(move |_| {
        renewing.set(false);
        spawn_local(async move {
            match api::post::<Registry>("/admin/game-servers/token", &()).await {
                Ok(found) => {
                    toast("ok", "New token made", "Give it to the servers that list themselves.");
                    data.set(Some(found));
                }
                Err(err) => toast_err("No new token was made", &err),
            }
        });
    });

    let rows = move || {
        data.with(|data| {
            let servers = data.iter().flat_map(|data| &data.servers).map(|server| {
                let (id, game_id) = (server.id, server.game_id);
                let players = if server.max_players == 0 { "—".to_string() } else { format!("{}/{}", server.players, server.max_players) };
                let checked = match (server.online, server.ping_ms) {
                    (true, Some(ms)) => {
                        let grade = if ms < 50 {
                            "good"
                        } else if ms < 120 {
                            "mid"
                        } else {
                            "poor"
                        };
                        view! { <span class=format!("ping {grade}")><i><b></b><b></b><b></b></i>{format!("{ms} ms")}</span> }.into_any()
                    }
                    (true, None) if server.self_listed => {
                        view! { <span class="hrow" style="gap:6px;flex-wrap:nowrap"><i class="dot ok"></i>"Reporting in"</span> }.into_any()
                    }
                    _ => view! { <span class="faint">"No answer"</span> }.into_any(),
                };
                view! {
                    <tr>
                        <td>
                            <div class="col" style="gap:0">
                                <b style="font-weight:500">{server.name.clone()}</b>
                                <span class="faint mono" style="font-size:11px">{server.address.clone()}</span>
                            </div>
                        </td>
                        // Titles may arrive after the servers, so the cell reads them when it draws.
                        <td class="muted">{move || game_id.map(|game| titles.with(|titles| title_of(titles, game)))}</td>
                        <td>{if server.self_listed { "Lists itself".to_string() } else { server.added_by.clone() }}</td>
                        <td class="num">{players}</td>
                        <td>{checked}</td>
                        <td class="act">
                            <button class="btn sm danger" on:click=move |_| delete_then(format!("/admin/game-servers/{id}"), "Not removed", move || load(true))>
                                "Remove"
                            </button>
                        </td>
                    </tr>
                }
            });
            servers.collect_view()
        })
    };

    view! {
        <div class="view">
            <div class="ph">
                <div>
                    <span class="lab">"Server registry, checked every 30 s"</span>
                    <h1 class="pt">"Firelink"</h1>
                </div>
            </div>
            <div class="card tblw">
                <table class="tbl">
                    <thead>
                        <tr>
                            <th>"Server"</th>
                            <th>"Game"</th>
                            <th>"Added by"</th>
                            <th class="num">"Players"</th>
                            <th>"Last check"</th>
                            <th></th>
                        </tr>
                    </thead>
                    <tbody>{rows}</tbody>
                </table>
                <Show when=move || data.with(|data| data.as_ref().is_some_and(|data| data.servers.is_empty()))>
                    <div class="empty">"No server is listed yet."</div>
                </Show>
            </div>
            <div class="cols even">
                <div class="card">
                    <header><h3>"Add a dedicated server"</h3></header>
                    <form class="pad col" style="gap:12px" on:submit=add>
                        <div class="formgrid">
                            <Field label="Game" target="fl-game"><Pick id="fl-game" value=game options=titles/></Field>
                            <Field label="Name" target="fl-name"><TextInput value=name id="fl-name" placeholder="Sunday factory"/></Field>
                            <Field label="Address" target="fl-address"><TextInput value=address id="fl-address" class="mono" placeholder="home.lan:34198"/></Field>
                        </div>
                        <ErrorNote message=error/>
                        <div><button class="btn pri">"Add server"</button></div>
                    </form>
                </div>
                <div class="card">
                    <header><h3>"Let servers report in"</h3></header>
                    <div class="pad col" style="gap:10px">
                        <span class="muted">
                            {format!(
                                "A server that posts here every {HEARTBEAT_SECONDS} s, with the token below as its bearer token, lists itself and drops off when it stops."
                            )}
                        </span>
                        <div class="code"><span>"POST /api/v1/game-servers/heartbeat"</span></div>
                        <CodeBox text=Signal::derive(move || data.with(|data| data.as_ref().map(|data| data.heartbeat_token.clone()).unwrap_or_default()))/>
                        <div><button class="btn sm" on:click=move |_| renewing.set(true)>"New token"</button></div>
                    </div>
                </div>
            </div>
        </div>
        <Show when=move || renewing.get()>
            <Confirm
                title="Make a new token"
                text="Servers that list themselves need the new token. Until they have it, they drop off the list."
                action="Make a new token"
                on_confirm=renew
                on_close=Callback::new(move |_| renewing.set(false))
            />
        </Show>
    }
}
