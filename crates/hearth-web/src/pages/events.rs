//! Game nights: when people play what together, and whether their PCs are ready for it.

use hearth_api::{
    Id,
    community::{GameNight, PlanEvent, Ready, ReadyCheck, Rsvp},
};
use leptos::{prelude::*, task::spawn_local};

use super::library::{load_titles, title_of};
use crate::{
    App, api,
    ui::{ErrorNote, Field, Icon, Pick, TextInput, day_time, delete_then, load_list, picked, toast, toast_err},
};

/// Unix seconds of a day and a time of day as the browser's date and time fields give them,
/// read in the browser's own time zone. `None` when one of the two is missing.
fn starts_at(day: &str, time: &str) -> Option<i64> {
    let at = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(&format!("{day}T{time}"))).get_time();
    (!at.is_nan()).then_some((at / 1000.0) as i64)
}

#[component]
pub fn Events() -> impl IntoView {
    let app = expect_context::<App>();
    let events = RwSignal::new(Vec::<GameNight>::new());
    let titles = RwSignal::new(Vec::<(Id, String)>::new());
    let (name, note) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let (day, time) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let game = RwSignal::new(None::<Id>);
    let error = RwSignal::new(String::new());

    let load = move || load_list("/events", events, "The events could not be loaded");
    load();
    load_titles(titles);

    let add = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let Some(game_id) = picked(game, &titles.get_untracked()) else {
            return error.set("There is no game to play yet. Add one to the library first.".into());
        };
        let Some(starts_at) = starts_at(&day.get_untracked(), &time.get_untracked()) else {
            return error.set("Pick the day and the time it starts.".into());
        };
        let plan = PlanEvent { title: name.get_untracked(), game_id, starts_at, note: note.get_untracked() };
        let hearth = app.name();
        spawn_local(async move {
            match api::post::<GameNight>("/admin/events", &plan).await {
                Ok(_) => {
                    name.set(String::new());
                    note.set(String::new());
                    error.set(String::new());
                    toast("ok", "Event added", format!("Everyone on {hearth} was notified."));
                    load();
                }
                Err(err) => error.set(err.message),
            }
        });
    };

    let rows = move || {
        events
            .get()
            .into_iter()
            .map(|event| {
                let (id, game_id) = (event.id, event.game_id);
                // Titles may arrive after the events, so the cells read them when they draw.
                let game = Signal::derive(move || titles.with(|titles| title_of(titles, game_id)));
                let going = event.answers.iter().filter(|answer| answer.rsvp == Rsvp::Going).count();
                view! {
                    <tr>
                        <td style="white-space:normal">
                            <b style="font-weight:500">{event.title}</b>
                            {(going > 0).then(|| view! { <Readiness event=id game=game/> })}
                        </td>
                        <td class="muted">{game}</td>
                        <td>{day_time(event.starts_at)}</td>
                        <td class="num">{going}</td>
                        <td class="act">
                            <button class="btn sm danger" on:click=move |_| delete_then(format!("/admin/events/{id}"), "Not removed", load)>"Remove"</button>
                        </td>
                    </tr>
                }
            })
            .collect_view()
    };

    view! {
        <div class="view">
            <div class="ph">
                <div>
                    <span class="lab">"Players answer in Bonfire"</span>
                    <h1 class="pt">"Events"</h1>
                </div>
            </div>
            <div class="card tblw">
                <table class="tbl">
                    <thead>
                        <tr>
                            <th>"Event"</th>
                            <th>"Game"</th>
                            <th>"When"</th>
                            <th class="num">"Going"</th>
                            <th></th>
                        </tr>
                    </thead>
                    <tbody>{rows}</tbody>
                </table>
                <Show when=move || events.with(Vec::is_empty)>
                    <div class="empty">"No game night is planned."</div>
                </Show>
            </div>
            <div class="card">
                <header><h3>"Plan a game night"</h3></header>
                <form class="pad col" style="gap:12px" on:submit=add>
                    <div class="formgrid">
                        <Field label="Name" target="ev-name"><TextInput value=name id="ev-name" placeholder="Rail Empire reset"/></Field>
                        <Field label="Game" target="ev-game"><Pick id="ev-game" value=game options=titles/></Field>
                        <Field label="Day" target="ev-day"><TextInput value=day id="ev-day" label="Day" kind="date"/></Field>
                        <Field label="Starts" target="ev-time"><TextInput value=time id="ev-time" label="Starts" kind="time"/></Field>
                    </div>
                    <Field label="Note" target="ev-note"><TextInput value=note id="ev-note" placeholder="Optional. What to bring, or where to meet."/></Field>
                    <ErrorNote message=error/>
                    <div class="hrow">
                        <button class="btn pri" disabled=move || name.with(|name| name.trim().is_empty())>"Add event"</button>
                        <span class="faint" style="font-size:12px">"Adding one notifies players."</span>
                    </div>
                </form>
            </div>
        </div>
    }
}

/// The ready check of one game night: who of the people going has the game, in the version
/// everyone needs.
#[component]
fn Readiness(event: Id, game: Signal<String>) -> impl IntoView {
    let check = RwSignal::new(None::<ReadyCheck>);
    spawn_local(async move {
        // Without an answer the event is still listed, only without its ready check.
        if let Ok(found) = api::get::<ReadyCheck>(&format!("/events/{event}/ready")).await {
            check.set(Some(found));
        }
    });
    let get_ready = move |_| {
        spawn_local(async move {
            match api::post::<ReadyCheck>(&format!("/admin/events/{event}/ready"), &()).await {
                Ok(found) => {
                    toast("ok", "Downloads queued", "Each one starts as soon as that PC comes online.");
                    check.set(Some(found));
                }
                Err(err) => toast_err("Nothing was queued", &err),
            }
        });
    };
    move || {
        check.get().filter(|check| !check.guests.is_empty()).map(|check| {
            let late = check.guests.iter().filter(|guest| guest.ready != Ready::Ready).count();
            let on_its_way = check.guests.iter().filter(|guest| guest.ready == Ready::GettingReady).count();
            let badges = check.guests.iter().map(|guest| {
                let (tone, icon, label) = match guest.ready {
                    Ready::Ready => ("ok", "check", "Ready"),
                    Ready::NeedsUpdate => ("warn", "download", "Needs an update"),
                    Ready::NotKindled => ("", "download", "Not kindled"),
                    Ready::GettingReady => ("ember", "refresh", "Getting ready"),
                };
                view! {
                    <span class=format!("badge {tone}") title=label>
                        {guest.username.clone()}
                        <Icon name=icon size=11 stroke=2.0/>
                        <span class="hide-s">{label}</span>
                    </span>
                }
            });
            let badges = badges.collect_view();
            let next = if late == 0 {
                let version = check.version.map(|version| version.name).unwrap_or_default();
                view! { <span class="faint" style="font-size:12px">{move || format!("Everyone has {} {version}", game.get()).trim_end().to_string()}</span> }
                    .into_any()
            } else if late == on_its_way {
                // Every download that is missing is already on its way.
                ().into_any()
            } else {
                view! { <button type="button" class="btn sm" on:click=get_ready>"Get everyone ready"</button> }.into_any()
            };
            view! { <div class="rc"><span class="lab">"Ready check"</span>{badges}{next}</div> }
        })
    }
}
