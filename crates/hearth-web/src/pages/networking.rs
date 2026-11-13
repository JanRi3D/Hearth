//! Bonfire networking: what the Steam API replacement is doing on this Hearth right now.

use hearth_api::{Id, admin::Networking as Data, library::GamePatch, multiplayer::Route, settings::SettingsPatch};
use leptos::{prelude::*, task::spawn_local};

use super::library::{load_titles, title_of};
use crate::{
    App, api,
    ui::{Avatar, Icon, duration, every, size, toast_err},
};

/// What the library does with each Steam interface: (interface, what happens, badge, verdict).
const IFACE: [(&str, &str, &str, &str); 12] = [
    ("ISteamUser", "Identity and session tickets, issued from the Hearth account", "ok", "Answered"),
    ("ISteamFriends", "Friends, presence, rich presence, invites and join requests", "ok", "Answered"),
    ("ISteamMatchmaking", "Lobbies, lobby data, lobby chat and lobby search", "ok", "Answered"),
    ("ISteamNetworkingSockets", "Peer-to-peer sessions. Direct first, Bonfire relay when a direct path fails", "ok", "Answered"),
    ("ISteamNetworkingMessages", "Unreliable and reliable messages over the same sessions", "ok", "Answered"),
    ("ISteamUserStats", "Achievements and stats, stored on the Hearth with playtime", "ok", "Answered"),
    ("ISteamApps", "Ownership and DLC, answered from the Hearth library", "ok", "Answered"),
    ("ISteamUtils", "App id, overlay state, server time", "ok", "Answered"),
    ("ISteamRemoteStorage", "Handed to Rest, which already backs up the save folder", "warn", "Partly"),
    ("ISteamNetworking", "Older peer-to-peer calls, mapped onto sockets", "warn", "Partly"),
    ("ISteamInput", "Passed straight to the system", "info", "Passed through"),
    ("ISteamUGC", "Workshop content is not served", "", "Not served"),
];

/// Tells the Hearth a switch's new position, then reads the page again. That also puts the
/// switch back when the Hearth did not take it.
fn send_switch(path: String, body: impl serde::Serialize + 'static, reload: impl FnOnce() + 'static) {
    spawn_local(async move {
        if let Err(err) = api::patch::<serde::de::IgnoredAny>(&path, &body).await {
            toast_err("Not saved", &err);
        }
        reload();
    });
}

fn replaced(data: &Option<Data>, game: Id) -> bool {
    data.iter().flat_map(|data| &data.games).any(|found| found.id == game && found.networking)
}

/// A time of day with seconds, in the browser's own time zone.
fn clock(at: i64) -> String {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(at as f64 * 1000.0));
    format!("{:02}:{:02}:{:02}", date.get_hours(), date.get_minutes(), date.get_seconds())
}

#[component]
pub fn Networking() -> impl IntoView {
    let app = expect_context::<App>();
    let data = RwSignal::new(None::<Data>);
    // A world can belong to any game, not only to those in the list of games played together.
    let titles = RwSignal::new(Vec::<(Id, String)>::new());
    load_titles(titles);
    // `quiet` is for the refresh every few seconds, which does not nag while the Hearth is away.
    let load = move |quiet: bool| {
        spawn_local(async move {
            match api::get::<Data>("/admin/networking").await {
                Ok(found) => {
                    app.lobbies.set(found.lobbies.len() as u32);
                    data.set(Some(found));
                }
                Err(err) if !quiet => toast_err("Bonfire networking could not be loaded", &err),
                Err(_) => {}
            }
        });
    };
    load(false);
    every(5, move || load(true));

    let relay_on = move || data.with(|data| data.as_ref().is_some_and(|data| data.relay_enabled));
    let flip_relay = move |_| {
        let now = !data.with_untracked(|data| data.as_ref().is_some_and(|data| data.relay_enabled));
        data.update(|data| {
            if let Some(data) = data {
                data.relay_enabled = now;
            }
        });
        send_switch("/admin/settings".into(), SettingsPatch { relay_enabled: Some(now), ..Default::default() }, move || load(true));
    };

    let kpis = move || {
        data.with(|data| {
            data.as_ref().map(|data| {
                let relayed = data.lobbies.iter().filter(|lobby| lobby.route == Route::Relay).count();
                let games = data.games.iter().filter(|game| game.networking).count();
                let mbit = data.relay_bits_per_second as f64 / 1_000_000.0;
                let (traffic, unit) = if mbit >= 0.1 { (format!("{mbit:.1}"), "Mbit/s") } else { (format!("{:.0}", mbit * 1000.0), "kbit/s") };
                let relay = match (data.relay_enabled, data.relay_port) {
                    (false, _) => "switched off".to_string(),
                    (true, Some(port)) => format!("udp {port}"),
                    (true, None) => "could not listen".to_string(),
                };
                let (calls, thousands) = if data.calls_today >= 10_000 {
                    (format!("{:.1}", data.calls_today as f64 / 1000.0), "k")
                } else {
                    (data.calls_today.to_string(), "")
                };
                // Two decimals at most, and none that say nothing: 0, 0.02, 1.5.
                let failed = format!("{:.2}", data.failed_today as f64 * 100.0 / data.calls_today.max(1) as f64);
                let failed = failed.trim_end_matches('0').trim_end_matches('.').to_string();
                view! {
                    <div class="kpis">
                        <div class="kpi">
                            <span class="lab">"Lobbies"</span>
                            <b>{data.lobbies.len()}<small>"open"</small></b>
                            <span>{format!("{relayed} on the relay")}</span>
                        </div>
                        <div class="kpi">
                            <span class="lab">"Games"</span>
                            <b>{games}<small>"replaced"</small></b>
                            <span>"original kept as .orig"</span>
                        </div>
                        <div class="kpi">
                            <span class="lab">"Relay traffic"</span>
                            <b>{traffic}<small>{unit}</small></b>
                            <span>{relay}</span>
                        </div>
                        <div class="kpi">
                            <span class="lab">"Calls today"</span>
                            <b>{calls}<small>{thousands}</small></b>
                            <span>{format!("{failed} % not served")}</span>
                        </div>
                    </div>
                }
            })
        })
    };

    let lobbies = move || {
        data.with(|data| {
            data.as_ref().map(|data| {
                let now = (js_sys::Date::now() / 1000.0) as i64;
                let rows = data.lobbies.iter().map(|lobby| {
                    let route = match lobby.route {
                        Route::Relay => view! { <span class="badge info"><Icon name="server" size=11 stroke=2.0/>"Relay"</span> }.into_any(),
                        Route::Direct => view! { <span class="badge ok"><Icon name="zap" size=11 stroke=2.0/>"Direct"</span> }.into_any(),
                        Route::Pending => view! { <span class="badge">"Connecting"</span> }.into_any(),
                    };
                    view! {
                        <tr>
                            <td><b style="font-weight:500">{lobby.name.clone()}</b></td>
                            <td class="muted">{lobby.game_title.clone()}</td>
                            <td>{lobby.owner.clone()}</td>
                            <td class="num">{format!("{}/{}", lobby.members, lobby.max_members)}</td>
                            <td>{route}</td>
                            <td class="muted">{duration((now - lobby.created_at).max(0) as u64)}</td>
                        </tr>
                    }
                });
                let body = if data.lobbies.is_empty() {
                    view! { <div class="empty">"No lobby is open."</div> }.into_any()
                } else {
                    view! {
                        <div class="tblw">
                            <table class="tbl" style="min-width:520px">
                                <thead>
                                    <tr>
                                        <th>"Lobby"</th>
                                        <th>"Game"</th>
                                        <th>"Host"</th>
                                        <th class="num">"Members"</th>
                                        <th>"Route"</th>
                                        <th>"Open for"</th>
                                    </tr>
                                </thead>
                                <tbody>{rows.collect_view()}</tbody>
                            </table>
                        </div>
                    }
                        .into_any()
                };
                view! {
                    <header><h3>"Open lobbies"</h3><span class="muted mono">{data.lobbies.len()}</span></header>
                    {body}
                }
            })
        })
    };

    let worlds = move || {
        data.with(|data| {
            data.as_ref().map(|data| {
                if data.worlds.is_empty() {
                    return view! { <div class="empty">"No shared world yet. Players make them in Bonfire."</div> }.into_any();
                }
                let rows = data.worlds.iter().map(|world| {
                    let game_id = world.game_id;
                    let members = world.members.iter().map(|member| view! { <Avatar name=member.username.clone() size=20/> }).collect_view();
                    let host = match &world.host {
                        Some(host) => view! { <span class="badge ok">{host.username.clone()}</span> }.into_any(),
                        None => view! { <span class="faint">"Free"</span> }.into_any(),
                    };
                    view! {
                        <tr>
                            <td><b style="font-weight:500">{world.name.clone()}</b></td>
                            <td class="muted">{move || titles.with(|titles| title_of(titles, game_id))}</td>
                            <td><span class="avs">{members}</span></td>
                            <td class="num">{if world.revision == 0 { "Empty".to_string() } else { size(world.bytes) }}</td>
                            <td>{host}</td>
                        </tr>
                    }
                });
                view! {
                    <div class="tblw">
                        <table class="tbl" style="min-width:520px">
                            <thead>
                                <tr>
                                    <th>"World"</th>
                                    <th>"Game"</th>
                                    <th>"Members"</th>
                                    <th class="num">"Size"</th>
                                    <th>"Host now"</th>
                                </tr>
                            </thead>
                            <tbody>{rows.collect_view()}</tbody>
                        </table>
                    </div>
                }
                    .into_any()
            })
        })
    };

    let calls = move || {
        data.with(|data| {
            data.as_ref().map(|data| {
                if data.recent_calls.is_empty() {
                    return view! { <div class="empty">"No calls yet. They show up here once a game with Bonfire networking runs."</div> }.into_any();
                }
                // The log keeps its spaces, so each call is a line of its own with two between the parts.
                let lines = data.recent_calls.iter().map(|call| {
                    let (tone, status) = if call.status < 400 { ("ok", "ok".to_string()) } else { ("wr", call.status.to_string()) };
                    view! {
                        <div>
                            <span class="t">{clock(call.at)}</span>
                            "  "
                            <span class="in">{call.interface.clone()}</span>
                            {format!("::{}  ", call.call)}
                            <span class=tone>{status}</span>
                            "  "
                            <span class="t">{format!("{} ms", call.millis)}</span>
                        </div>
                    }
                });
                view! { <div class="log">{lines.collect_view()}</div> }.into_any()
            })
        })
    };

    let game_row = move |(id, title): (Id, String)| {
        let on = move || data.with(|data| replaced(data, id));
        let flip = move |_| {
            let now = !data.with_untracked(|data| replaced(data, id));
            data.update(|data| {
                if let Some(game) = data.iter_mut().flat_map(|data| &mut data.games).find(|game| game.id == id) {
                    game.networking = now;
                }
            });
            send_switch(format!("/admin/games/{id}"), GamePatch { networking: Some(now), ..Default::default() }, move || load(true));
        };
        view! {
            <div class="row" style="min-height:48px">
                <span class="gico"></span>
                <div class="col">
                    <span class="t">{title.clone()}</span>
                    <span class="d">
                        {move || {
                            if on() {
                                "steam_api64.dll replaced · original kept as steam_api64.dll.orig"
                            } else {
                                "Nothing replaced. Firelink lists its servers."
                            }
                        }}
                    </span>
                </div>
                <button
                    type="button"
                    class="sw"
                    role="switch"
                    aria-checked=move || on().to_string()
                    aria-label=format!("Bonfire networking for {title}")
                    on:click=flip
                ></button>
            </div>
        }
    };

    view! {
        <div class="view">
            <div class="ph">
                <div style="flex:1 1 420px">
                    <span class="lab">"The steam_api64.dll replacement"</span>
                    <h1 class="pt">"Bonfire networking"</h1>
                    <p class="sub" style="max-width:76ch">
                        {move || {
                            format!(
                                "Games call the Steam API as usual. Bonfire's library answers, and {} does the work Steam's servers would: identity, friends, lobbies, invites, peer-to-peer and achievements.",
                                app.name()
                            )
                        }}
                    </p>
                </div>
                <div class="tools">
                    <span class="muted">"Bonfire relay"</span>
                    <button
                        type="button"
                        class="sw"
                        role="switch"
                        aria-checked=move || relay_on().to_string()
                        aria-label="Bonfire relay"
                        on:click=flip_relay
                    ></button>
                </div>
            </div>
            {kpis}
            <div class="cols">
                <div class="col" style="gap:20px">
                    <div class="card">{lobbies}</div>
                    <div class="card">
                        <header>
                            <h3>"Shared worlds"</h3>
                            <span class="muted">{move || format!("saves kept on {}, so any member can host", app.name())}</span>
                        </header>
                        {worlds}
                    </div>
                    <div class="card">
                        <header><h3>"Per game"</h3><span class="muted">{move || format!("Applies to everyone on {}", app.name())}</span></header>
                        <div class="rows">
                            // A row is only drawn again when its game is another one, so a refresh
                            // leaves the switch someone is on alone.
                            <For
                                each=move || data.with(|data| data.iter().flat_map(|data| &data.games).map(|game| (game.id, game.title.clone())).collect::<Vec<_>>())
                                key=|game| game.clone()
                                children=game_row
                            />
                            {move || {
                                data.with(|data| data.as_ref().is_some_and(|data| data.games.is_empty()))
                                    .then(|| view! { <div class="empty">"No game in the library is played together."</div> })
                            }}
                        </div>
                    </div>
                    <div class="card">
                        <header><h3>"Recent calls"</h3></header>
                        {calls}
                    </div>
                </div>
                <div class="card">
                    <header><h3>"What the library answers"</h3></header>
                    <div class="rows">
                        {IFACE
                            .iter()
                            .map(|(name, what, badge, verdict)| {
                                view! {
                                    <div class="row" style="align-items:flex-start;padding:10px 14px">
                                        <div class="col">
                                            <span class="t mono" style="font-size:12px">{*name}</span>
                                            <span class="d">{*what}</span>
                                        </div>
                                        <span class=format!("badge {badge}")>{*verdict}</span>
                                    </div>
                                }
                            })
                            .collect_view()}
                    </div>
                </div>
            </div>
        </div>
    }
}
