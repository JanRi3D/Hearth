//! Emulators: stored once on the Hearth and shared by every game that needs them.

use std::collections::BTreeSet;

use hearth_api::community::{Emulator, EmulatorInput};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api,
    ui::{Confirm, ErrorNote, Field, Icon, TextInput, delete_then, every, load_list, size, size_parts, toast},
};

#[component]
pub fn Emulators() -> impl IntoView {
    let emulators = RwSignal::new(Vec::<Emulator>::new());
    let removing = RwSignal::new(None::<Emulator>);
    let (name, systems, version) = (RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()));
    let (command, folder) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let error = RwSignal::new(String::new());

    let load = move || load_list("/emulators", emulators, "The emulators could not be loaded");
    load();
    // Where the files go, so nobody has to look it up in Settings first.
    let base = RwSignal::new(String::new());
    spawn_local(async move {
        if let Ok(view) = api::get::<hearth_api::settings::SettingsView>("/admin/settings").await {
            base.set(view.settings.emulators_folder);
        }
    });
    // A new emulator has no size until the Hearth has hashed its files. Until then the list is
    // read again every two seconds, without a word when that fails.
    every(2, move || {
        if emulators.with_untracked(|list| list.iter().any(|emulator| !emulator.ready)) {
            spawn_local(async move {
                if let Ok(found) = api::get::<Vec<Emulator>>("/emulators").await {
                    emulators.set(found);
                }
            });
        }
    });

    let add = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let input = EmulatorInput {
            name: name.get_untracked(),
            version: version.get_untracked(),
            systems: systems.get_untracked().split(',').map(|system| system.trim().to_string()).filter(|system| !system.is_empty()).collect(),
            command: command.get_untracked(),
            folder: folder.get_untracked(),
        };
        spawn_local(async move {
            match api::post::<Emulator>("/admin/emulators", &input).await {
                Ok(added) => {
                    for field in [name, systems, version, command, folder] {
                        field.set(String::new());
                    }
                    error.set(String::new());
                    toast("ok", "Emulator added", format!("Pick {} for a game in Library, under Edit.", added.name));
                    load();
                }
                Err(err) => error.set(err.message),
            }
        });
    };

    let kpis = move || {
        emulators.with(|list| {
            let systems = list.iter().flat_map(|emulator| &emulator.systems).collect::<BTreeSet<_>>().len();
            let games: u32 = list.iter().map(|emulator| emulator.games).sum();
            // Every game after the first that uses an emulator does not bring it along again.
            let (saved, unit) = size_parts(list.iter().map(|emulator| emulator.bytes * u64::from(emulator.games.saturating_sub(1))).sum());
            view! {
                <div class="kpis three">
                    <div class="kpi">
                        <span class="lab">"Emulators"</span>
                        <b>{list.len()}</b>
                        <span>{format!("{systems} {}", if systems == 1 { "system" } else { "systems" })}</span>
                    </div>
                    <div class="kpi">
                        <span class="lab">"Games using them"</span>
                        <b>{games}</b>
                        <span>"ship only their own files"</span>
                    </div>
                    <div class="kpi">
                        <span class="lab">"Saved per player"</span>
                        <b>{saved}<small>{unit}</small></b>
                        <span>"never downloaded a second time"</span>
                    </div>
                </div>
            }
        })
    };

    let rows = move || {
        emulators
            .get()
            .into_iter()
            .map(|emulator| {
                let systems = if emulator.systems.is_empty() { "—".to_string() } else { emulator.systems.join(", ") };
                let bytes = if emulator.ready { size(emulator.bytes) } else { "Preparing".to_string() };
                let games = match emulator.games {
                    0 => view! { <span class="faint">"None yet"</span> }.into_any(),
                    count => view! { <span class="mono muted">{count}</span> }.into_any(),
                };
                view! {
                    <tr>
                        <td>
                            <span class="hrow" style="flex-wrap:nowrap">
                                <span class="tico" style="width:24px;height:24px"><Icon name="chip" size=13/></span>
                                <b style="font-weight:500">{emulator.name.clone()}</b>
                            </span>
                        </td>
                        <td class="mono">{emulator.version.clone()}</td>
                        <td class="muted">{systems}</td>
                        <td class="num">{bytes}</td>
                        <td>{games}</td>
                        <td class="mono" style="font-size:12px">{emulator.command.clone()}</td>
                        <td class="act"><button class="btn sm danger" on:click=move |_| removing.set(Some(emulator.clone()))>"Remove"</button></td>
                    </tr>
                }
            })
            .collect_view()
    };

    let remove_dialog = move || {
        removing.get().map(|emulator| {
            let id = emulator.id;
            let confirm = Callback::new(move |_| {
                removing.set(None);
                delete_then(format!("/admin/emulators/{id}"), "Not removed", load);
            });
            view! {
                <Confirm
                    title=format!("Remove {}", emulator.name)
                    text=format!("{} leaves this list, and games that used it go back to having no emulator. Its files stay on disk.", emulator.name)
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
                <div style="flex:1 1 420px">
                    <span class="lab">"Stored once, shared by every game that needs them"</span>
                    <h1 class="pt">"Emulators"</h1>
                    <p class="sub" style="max-width:76ch">
                        "A game for an older system points at an emulator instead of carrying its own copy. A player downloads the emulator with the first such game, and only the game files after that."
                    </p>
                </div>
            </div>
            {kpis}
            <div class="card tblw">
                <table class="tbl">
                    <thead>
                        <tr>
                            <th>"Emulator"</th>
                            <th>"Version"</th>
                            <th>"Systems"</th>
                            <th class="num">"Size"</th>
                            <th>"Games"</th>
                            <th>"Starts a game with"</th>
                            <th></th>
                        </tr>
                    </thead>
                    <tbody>{rows}</tbody>
                </table>
                <Show when=move || emulators.with(Vec::is_empty)>
                    <div class="empty">"No emulator yet. Add the first one below."</div>
                </Show>
            </div>
            <div class="cols even">
                <div class="card">
                    <header><h3>"Add an emulator"</h3></header>
                    <form class="pad col" style="gap:12px" on:submit=add>
                        <div class="formgrid">
                            <Field label="Name" target="em-name"><TextInput value=name id="em-name" placeholder="DuckStation"/></Field>
                            <Field label="Systems" target="em-systems"><TextInput value=systems id="em-systems" placeholder="PlayStation, PlayStation 2"/></Field>
                            <Field label="Version" target="em-version"><TextInput value=version id="em-version" class="mono" placeholder="0.1-7294"/></Field>
                            <Field label="Folder" target="em-folder"><TextInput value=folder id="em-folder" class="mono" placeholder="Named like the emulator"/></Field>
                        </div>
                        <Field label="Starts a game with" target="em-command">
                            <TextInput value=command id="em-command" class="mono" placeholder="duckstation.exe -fullscreen \"{rom}\""/>
                        </Field>
                        <ErrorNote message=error/>
                        <div class="hrow">
                            <button class="btn pri">"Add emulator"</button>
                            <span class="faint" style="font-size:12px;flex:1 1 220px">
                                "Put its files into a folder inside "
                                <span class="mono" style="overflow-wrap:anywhere">{move || Some(base.get()).filter(|base| !base.is_empty()).unwrap_or_else(|| "the emulators folder".into())}</span>
                                " first. Leave Folder empty when that folder is named like the emulator. {rom} stands for the game file."
                            </span>
                        </div>
                    </form>
                </div>
                <div class="card">
                    <header><h3>"How a game uses one"</h3></header>
                    <div class="pad col" style="gap:12px">
                        <span class="muted">
                            "In Library, open a game with Edit and pick its emulator. The game then ships only its own files, and every player keeps one copy of the emulator for all of them."
                        </span>
                        <div class="route">
                            <div class="node"><Icon name="box" size=15/><span>"Game files"</span></div>
                            <div class="wire"><span>"points at"</span></div>
                            <div class="node hot"><Icon name="chip" size=15/><span>"Emulator"</span></div>
                            <div class="wire live"><span>"once per PC"</span></div>
                            <div class="node"><Icon name="pad" size=15/><span>"Player"</span></div>
                        </div>
                    </div>
                </div>
            </div>
        </div>
        {remove_dialog}
    }
}
