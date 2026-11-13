//! Every game the Hearth found in its games folders: what players see of it, and how it starts.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use hearth_api::{
    Id,
    community::{Emulator, GameServer},
    library::{Game, GamePatch, GameSummary, LaunchOption, Library as Data, Players, Prerequisite, ScanStatus, SourceKind, VersionPatch},
    people::{Device, DownloadState, QueueDownload, QueuedDownload},
};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    App, api,
    ui::{Choice, Dialog, ErrorNote, Field, Icon, Pick, TextInput, Toggle, delete_then, load_list, picked, size, sleep, toast, toast_err, when},
};

/// Every game's id and title, for the pages that name games or let one be picked.
pub fn load_titles(into: RwSignal<Vec<(Id, String)>>) {
    spawn_local(async move {
        match api::get::<Data>("/admin/library").await {
            Ok(found) => into.set(found.games.into_iter().map(|game| (game.id, game.title)).collect()),
            Err(err) => toast_err("The games could not be loaded", &err),
        }
    });
}

/// The title that goes with a game's id, or nothing when the game is not in the list.
pub fn title_of(titles: &[(Id, String)], game: Id) -> String {
    titles.iter().find(|(id, _)| *id == game).map(|(_, title)| title.clone()).unwrap_or_default()
}

// ---------- scans ----------

/// What a page knows about scans of the games folders.
#[derive(Clone, Copy)]
pub struct ScanWatch {
    /// The scan as last read. `running` is true from the moment a scan was asked for.
    pub scan: RwSignal<ScanStatus>,
    /// Reads the page's data again.
    pub refresh: Callback<()>,
    /// Follows a scan that was just asked for until it is done.
    pub follow: Callback<()>,
}

/// Reads a page's data now, and again about once a second while a scan runs or versions are
/// still being hashed. `load` reads and stores the data and answers with the scan in it, or
/// with `None` when that failed. It can finish once more after the page was left, so it may
/// set the page's signals but must not read them.
pub fn watch_scan<F, Fut>(load: F) -> ScanWatch
where
    F: Fn() -> Fut + Copy + Send + Sync + 'static,
    Fut: Future<Output = Option<ScanStatus>> + 'static,
{
    let scan = RwSignal::new(ScanStatus::default());
    // The `scans_finished` that a scan someone asked for has to reach. A request does not
    // start the scan at once, so `running` alone would end the wait too early.
    // ponytail: a Hearth that restarts in the middle counts from zero again, so the page keeps
    // asking until it is left. Drop the wait when `scans_finished` goes backwards if that bites.
    let wanted = StoredValue::new(0u64);
    // Each run of the loop below takes a number, and a newer run ends the one before it.
    let turn = StoredValue::new(0u32);
    let left = Arc::new(AtomicBool::new(false));
    on_cleanup({
        let left = left.clone();
        move || left.store(true, Ordering::Relaxed)
    });
    let refresh = Callback::new(move |_: ()| {
        let left = left.clone();
        let mine = turn.get_value() + 1;
        turn.set_value(mine);
        spawn_local(async move {
            // Leaving the page takes its signals away, so that is asked first.
            let current = move || !left.load(Ordering::Relaxed) && turn.get_value() == mine;
            while current() {
                let found = load().await;
                if !current() {
                    return;
                }
                let Some(mut found) = found else {
                    // Nothing more is known, so the page stops saying that a scan runs.
                    return scan.update(|scan| scan.running = false);
                };
                found.running |= found.scans_finished < wanted.get_value();
                let busy = found.running || found.hashing > 0;
                scan.set(found);
                if !busy {
                    return;
                }
                sleep(1000).await;
            }
        });
    });
    let follow = Callback::new(move |_: ()| {
        wanted.set_value(scan.with_untracked(|scan| scan.scans_finished) + 1);
        scan.update(|scan| {
            scan.running = true;
            scan.files = 0;
        });
        refresh.run(());
    });
    refresh.run(());
    ScanWatch { scan, refresh, follow }
}

/// Asks for a scan of the games folders. While one runs it counts the files looked at so far.
#[component]
pub fn RescanButton(watch: ScanWatch, idle: &'static str, #[prop(default = "")] unit: &'static str) -> impl IntoView {
    let rescan = move |_| {
        spawn_local(async move {
            match api::post::<()>("/admin/library/scan", &()).await {
                // The page may have been left while the request was on its way.
                Ok(()) => {
                    let _ = watch.follow.try_run(());
                }
                Err(err) => toast_err("The scan did not start", &err),
            }
        });
    };
    view! {
        <button class="btn" disabled=move || watch.scan.with(|scan| scan.running) on:click=rescan>
            <Icon name="refresh" size=14/>
            {move || watch.scan.with(|scan| if scan.running { format!("Scanning · {}{unit}", scan.files) } else { idle.to_string() })}
        </button>
    }
}

// ---------- the page ----------

#[component]
pub fn Library() -> impl IntoView {
    let app = expect_context::<App>();
    let data = RwSignal::new(None::<Data>);
    let filter = RwSignal::new(String::new());
    // The game in the editor: its id, and its title for the dialog's heading.
    let editing = RwSignal::new(None::<(Id, String)>);
    // "Uploading name · 42 %" while a file is on its way.
    let uploading = RwSignal::new(None::<String>);
    let file_input = NodeRef::<leptos::html::Input>::new();
    // Firelink's registry, for the number of servers next to each game.
    let servers = RwSignal::new(Vec::<GameServer>::new());
    load_list("/game-servers", servers, "The Firelink servers could not be loaded");

    let watch = watch_scan(move || async move {
        match api::get::<Data>("/admin/library").await {
            Ok(found) => {
                app.library.update(|stats| stats.review = found.review);
                let scan = found.scan.clone();
                data.set(Some(found));
                Some(scan)
            }
            Err(err) => {
                toast_err("The library could not be loaded", &err);
                None
            }
        }
    });

    let upload = move |_| {
        let Some(input) = file_input.get_untracked() else { return };
        let Some(file) = input.files().and_then(|files| files.get(0)) else { return };
        // Emptied, so that choosing the same file again counts as a change.
        input.set_value("");
        let name = file.name();
        uploading.set(Some(format!("Uploading {name}")));
        spawn_local(async move {
            let path = format!("/admin/library/upload?name={}", api::encode(&name));
            let shown = name.clone();
            let sent = api::upload(&path, &file, move |percent| uploading.set(Some(format!("Uploading {shown} · {percent} %")))).await;
            uploading.set(None);
            match sent {
                Ok(()) => {
                    toast("ok", "Uploaded", format!("{name} shows up here once the Hearth has read it, or in Metadata review if it needs a match."));
                    let _ = watch.follow.try_run(());
                }
                Err(err) => toast_err("The upload did not finish", &err),
            }
        });
    };

    let label = move || {
        let folders = app.library.with(|stats| stats.folders.join(", "));
        if folders.is_empty() { "Every game in your games folders".to_string() } else { format!("Every game in {folders}") }
    };
    let sub = move || {
        data.with(|data| {
            data.as_ref().map(|data| {
                format!(
                    "{} {} · {} · {} hidden from players",
                    data.games.len(),
                    if data.games.len() == 1 { "game" } else { "games" },
                    size(data.games.iter().map(|game| game.bytes).sum()),
                    data.games.iter().filter(|game| !game.visible).count()
                )
            })
        })
    };
    let shown = move || {
        let wanted = filter.get().trim().to_lowercase();
        data.with(|data| {
            let games = data.iter().flat_map(|data| &data.games);
            games.filter(|game| wanted.is_empty() || game.title.to_lowercase().contains(&wanted)).cloned().collect::<Vec<_>>()
        })
    };
    let empty = move || {
        (data.with(Option::is_some) && shown().is_empty()).then(|| {
            let text = if filter.with(|filter| filter.trim().is_empty()) {
                "No games yet. Copy a game into a games folder or upload one, and it shows up here."
            } else {
                "No game matches that filter."
            };
            view! { <div class="empty">{text}</div> }
        })
    };
    // For the Game select below the table. A memo, so a poll that changes nothing leaves an
    // open select alone.
    let titles =
        Memo::new(move |_| data.with(|data| data.iter().flat_map(|data| &data.games).map(|game| (game.id, game.title.clone())).collect::<Vec<_>>()));
    let close = Callback::new(move |_: ()| editing.set(None));
    let saved = Callback::new(move |_: ()| {
        editing.set(None);
        watch.refresh.run(());
    });

    view! {
        <div class="view">
            <div class="ph">
                <div>
                    <span class="lab">{label}</span>
                    <h1 class="pt">"Library"</h1>
                    <p class="sub">{sub}</p>
                </div>
                <div class="tools">
                    <TextInput value=filter placeholder="Filter games"/>
                    <RescanButton watch=watch idle="Rescan"/>
                    <button
                        class="btn"
                        style="max-width:320px"
                        disabled=move || uploading.with(Option::is_some)
                        on:click=move |_| {
                            if let Some(input) = file_input.get_untracked() {
                                input.click();
                            }
                        }
                    >
                        <Icon name="upload" size=14/>
                        <span class="ell">{move || uploading.get().unwrap_or_else(|| "Upload".into())}</span>
                    </button>
                    <input type="file" accept=".zip,.7z,.rar,.exe,.iso" hidden=true node_ref=file_input on:change=upload/>
                </div>
            </div>
            <div class="card tblw">
                <table class="tbl">
                    <thead>
                        <tr>
                            <th>"Game"</th>
                            <th>"Developer"</th>
                            <th>"Version"</th>
                            <th class="num">"Size"</th>
                            <th class="num">"Firelink"</th>
                            <th>"Networking"</th>
                            <th>"Players see it"</th>
                            <th></th>
                        </tr>
                    </thead>
                    <tbody>
                        <For each=shown key=row_key children=move |game| view! { <GameRow game=game data=data servers=servers editing=editing/> }/>
                    </tbody>
                </table>
                {empty}
            </div>
            <KindleCard games=titles/>
        </div>
        {move || editing.get().map(|(id, title)| view! { <Editor id=id title=title on_close=close on_saved=saved/> })}
    }
}

// ---------- kindling on someone's PC ----------

/// Queues a game for one of the PCs people are signed in on. It starts when that PC is online.
#[component]
fn KindleCard(#[prop(into)] games: Signal<Vec<(Id, String)>>) -> impl IntoView {
    let devices = RwSignal::new(Vec::<Device>::new());
    let queue = RwSignal::new(Vec::<QueuedDownload>::new());
    let (game, device) = (RwSignal::new(None::<Id>), RwSignal::new(None::<Id>));
    let load_queue = move || load_list("/admin/downloads", queue, "The queued downloads could not be loaded");
    load_list("/admin/devices", devices, "The PCs could not be loaded");
    load_queue();

    // A browser on the admin pages is a device too, but nothing can be kindled on it.
    let pcs = Memo::new(move |_| {
        devices.with(|devices| {
            let pcs = devices.iter().filter(|device| device.client != "Web admin");
            pcs.map(|device| {
                let seen = if device.online { "online".to_string() } else { format!("last online {}", when(device.last_seen_at)) };
                (device.id, format!("{} · {} · {seen}", device.username, device.name))
            })
            .collect::<Vec<_>>()
        })
    });
    let add = move |_| {
        let (Some(game_id), Some(device_id)) = (picked(game, &games.get_untracked()), picked(device, &pcs.get_untracked())) else {
            return toast("warn", "Nothing to queue", "It takes a game in the library and a PC that is signed in with Bonfire.");
        };
        spawn_local(async move {
            match api::post::<QueuedDownload>("/admin/downloads", &QueueDownload { device_id, game_id }).await {
                Ok(_) => load_queue(),
                Err(err) => toast_err("Not queued", &err),
            }
        });
    };
    let waiting = move || {
        let queue = queue.get();
        if queue.is_empty() {
            return view! { <span class="faint" style="font-size:12px">"Nothing waiting. A queued game starts as soon as that PC comes online."</span> }
                .into_any();
        }
        let chips = queue.into_iter().map(|item| {
            let id = item.id;
            let text = if item.state == DownloadState::Started {
                format!("{} is kindling on {}", item.game_title, item.device_name)
            } else {
                format!("{} waits for {}", item.game_title, item.device_name)
            };
            view! {
                <span class="chip">
                    {text.clone()}
                    <button
                        class="ib"
                        style="width:18px;height:18px"
                        aria-label=format!("Cancel: {text}")
                        on:click=move |_| delete_then(format!("/admin/downloads/{id}"), "Not cancelled", load_queue)
                    >
                        <Icon name="x" size=11/>
                    </button>
                </span>
            }
        });
        chips.collect_view().into_any()
    };

    view! {
        <div class="card">
            <header><h3>"Kindle on one of your PCs"</h3></header>
            <div class="pad col" style="gap:12px">
                <div class="formgrid">
                    <Field label="Game" target="queue-game"><Pick id="queue-game" value=game options=games/></Field>
                    <Field label="PC" target="queue-pc"><Pick id="queue-pc" value=device options=pcs/></Field>
                    <button class="btn" on:click=add>"Queue download"</button>
                </div>
                <div class="hrow">{waiting}</div>
            </div>
        </div>
    }
}

/// What a row shows, apart from its switch. A row is drawn again only when this changes, so
/// reading the library during a scan does not take the keyboard focus out of the table.
fn row_key(game: &GameSummary) -> String {
    serde_json::to_string(&GameSummary { visible: true, ..game.clone() }).unwrap_or_default()
}

#[component]
fn GameRow(
    game: GameSummary,
    data: RwSignal<Option<Data>>,
    servers: RwSignal<Vec<GameServer>>,
    editing: RwSignal<Option<(Id, String)>>,
) -> impl IntoView {
    let id = game.id;
    // Read from the signal, because the registry may arrive after the row was drawn.
    let firelink = move || match servers.with(|servers| servers.iter().filter(|server| server.game_id == Some(id)).count()) {
        0 => "—".to_string(),
        count => count.to_string(),
    };
    let visible = RwSignal::new(game.visible);
    let show = Callback::new(move |_: ()| {
        let now = visible.get_untracked();
        spawn_local(async move {
            match api::patch::<Game>(&format!("/admin/games/{id}"), &GamePatch { visible: Some(now), ..Default::default() }).await {
                // The row keeps its switch. Only the count above the table follows.
                Ok(_) => data.update(|data| {
                    if let Some(game) = data.iter_mut().flat_map(|data| &mut data.games).find(|game| game.id == id) {
                        game.visible = now;
                    }
                }),
                Err(err) => {
                    visible.set(!now);
                    toast_err("Not saved", &err);
                }
            }
        });
    });
    let kind = match game.kind {
        SourceKind::Folder => None,
        SourceKind::Archive => Some("Archive"),
        SourceKind::Installer => Some("Installer"),
        SourceKind::Image => Some("Disc image"),
    };
    let cover = game.cover.map(|path| format!("background-image:url('{path}')"));
    let version = match game.version {
        Some(version) => view! { <td class="mono">{version.name}</td> }.into_any(),
        None => view! { <td class="muted">"Preparing"</td> }.into_any(),
    };
    let networking = if game.players == Players::Single {
        view! { <span class="faint">"—"</span> }.into_any()
    } else if game.networking {
        view! { <span class="badge ok">"Library replaced"</span> }.into_any()
    } else {
        view! { <span class="badge">"Own server browser"</span> }.into_any()
    };
    let title = game.title.clone();
    view! {
        <tr>
            <td>
                <span class="hrow" style="flex-wrap:nowrap">
                    <span class="gico" style=cover></span>
                    <b style="font-weight:500">{game.title.clone()}</b>
                    {kind.map(|kind| view! { <span class="badge">{kind}</span> })}
                </span>
            </td>
            <td class="muted">{game.developer}</td>
            {version}
            <td class="num">{size(game.bytes)}</td>
            <td class="num">{firelink}</td>
            <td>{networking}</td>
            <td><Toggle checked=visible label=format!("Players see {}", game.title) on_commit=show/></td>
            <td class="act"><button class="btn sm" on:click=move |_| editing.set(Some((id, title.clone())))>"Edit"</button></td>
        </tr>
    }
}

// ---------- the game editor ----------

const PLAYERS: [(Players, &str); 3] =
    [(Players::Single, "Single-player"), (Players::Coop, "Co-op"), (Players::Online, "Online with dedicated servers")];

/// The dialog behind Edit. It reads the game, then shows the form.
#[component]
fn Editor(id: Id, title: String, on_close: Callback<()>, on_saved: Callback<()>) -> impl IntoView {
    let game = RwSignal::new(None::<Game>);
    let error = RwSignal::new(String::new());
    spawn_local(async move {
        match api::get::<Game>(&format!("/admin/games/{id}")).await {
            Ok(found) => game.set(Some(found)),
            Err(err) => error.set(err.message),
        }
    });
    view! {
        <Dialog title=title on_close=on_close style="width:min(640px,100%)">
            {move || match game.get() {
                Some(game) => view! { <GameForm game=game on_close=on_close on_saved=on_saved/> }.into_any(),
                None => view! { <div class="db"><ErrorNote message=error/></div> }.into_any(),
            }}
        </Dialog>
    }
}

/// A number that may be left out, as typed: `Some(None)` for an empty field, `None` when
/// what is in the field is not a number.
fn optional<T: std::str::FromStr>(text: &str) -> Option<Option<T>> {
    let text = text.trim();
    if text.is_empty() { Some(None) } else { text.parse().ok().map(Some) }
}

#[component]
fn GameForm(game: Game, on_close: Callback<()>, on_saved: Callback<()>) -> impl IntoView {
    let id = game.summary.id;
    let title = RwSignal::new(game.summary.title);
    let developer = RwSignal::new(game.summary.developer);
    let year = RwSignal::new(game.summary.year.map(|year| year.to_string()).unwrap_or_default());
    let genre = RwSignal::new(game.summary.genre);
    let players = RwSignal::new(game.summary.players);
    let description = RwSignal::new(game.description);
    let steam_app_id = RwSignal::new(game.steam_app_id.map(|id| id.to_string()).unwrap_or_default());
    // The select lists "None" under the id 0, which no emulator has.
    let emulator = RwSignal::new(Some(game.summary.emulator_id.unwrap_or(0)));
    let emulators = RwSignal::new(Vec::<Emulator>::new());
    load_list("/emulators", emulators, "The emulators could not be loaded");
    let emulator_choices = Signal::derive(move || {
        let listed = emulators.with(|emulators| emulators.iter().map(|emulator| (emulator.id, emulator.name.clone())).collect::<Vec<_>>());
        [vec![(0, "None".to_string())], listed].concat()
    });
    let launch = RwSignal::new(game.launch);
    // What runs before the first start has the fields of a launch line, so it is edited as one.
    let prerequisites = RwSignal::new(
        game.prerequisites
            .into_iter()
            .map(|item| LaunchOption { name: item.name, path: item.path, args: item.args, default: false })
            .collect::<Vec<_>>(),
    );
    // Every version, with its name and note as they stand in the form.
    let versions = StoredValue::new(
        game.versions
            .into_iter()
            .map(|version| {
                let (name, note) = (RwSignal::new(version.name.clone()), RwSignal::new(version.note.clone()));
                (version, name, note)
            })
            .collect::<Vec<_>>(),
    );
    let error = RwSignal::new(String::new());
    let saving = RwSignal::new(false);
    let cover = format!("width:92px;flex:none{}", game.summary.cover.map(|path| format!(";background-image:url('{path}')")).unwrap_or_default());

    let save = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let (year, steam_app_id) = match (optional::<i32>(&year.get_untracked()), optional::<u32>(&steam_app_id.get_untracked())) {
            (Some(year), Some(steam_app_id)) => (year, steam_app_id),
            (None, _) => return error.set("Released is a year, for example 2019. Leave it empty if you do not know it.".into()),
            (_, None) => return error.set("The Steam app id is a number, for example 427520. Leave it empty if the game has none.".into()),
        };
        // A line that was added and left empty is not sent.
        let lines = |rows: RwSignal<Vec<LaunchOption>>| {
            rows.get_untracked().into_iter().filter(|line| !(line.name.trim().is_empty() && line.path.trim().is_empty())).collect::<Vec<_>>()
        };
        let patch = GamePatch {
            title: Some(title.get_untracked()),
            developer: Some(developer.get_untracked()),
            year: Some(year),
            genre: Some(genre.get_untracked()),
            players: Some(players.get_untracked()),
            description: Some(description.get_untracked()),
            steam_app_id: Some(steam_app_id),
            emulator_id: Some(emulator.get_untracked().filter(|id| *id != 0)),
            launch: Some(lines(launch)),
            prerequisites: Some(
                lines(prerequisites).into_iter().map(|line| Prerequisite { name: line.name, path: line.path, args: line.args }).collect(),
            ),
            ..Default::default()
        };
        // Only the versions whose name or note was changed.
        let renamed = versions.with_value(|versions| {
            versions
                .iter()
                .filter_map(|(version, name, note)| {
                    let patch = VersionPatch {
                        name: Some(name.get_untracked()).filter(|name| *name != version.name),
                        note: Some(note.get_untracked()).filter(|note| *note != version.note),
                    };
                    (patch.name.is_some() || patch.note.is_some()).then_some((version.id, patch))
                })
                .collect::<Vec<_>>()
        });
        error.set(String::new());
        saving.set(true);
        spawn_local(async move {
            let mut saved = api::patch::<Game>(&format!("/admin/games/{id}"), &patch).await.map(drop);
            for (version, patch) in renamed {
                if saved.is_ok() {
                    saved = api::patch::<()>(&format!("/admin/versions/{version}"), &patch).await;
                }
            }
            saving.set(false);
            match saved {
                Ok(()) => {
                    toast("ok", "Saved", "");
                    // The dialog may have been closed while the request was on its way.
                    let _ = on_saved.try_run(());
                }
                Err(err) => error.set(err.message),
            }
        });
    };

    view! {
        <form on:submit=save style="display:contents">
            <div class="db">
                <div class="hrow" style="align-items:flex-start;gap:14px">
                    <span class="cover" style=cover></span>
                    <div class="formgrid" style="flex:1 1 300px">
                        <Field label="Title" target="ed-title"><TextInput value=title id="ed-title" label="Title"/></Field>
                        <Field label="Developer" target="ed-developer"><TextInput value=developer id="ed-developer" label="Developer"/></Field>
                        <Field label="Released" target="ed-year"><TextInput value=year id="ed-year" placeholder="Year"/></Field>
                        <Field label="Genre" target="ed-genre"><TextInput value=genre id="ed-genre" label="Genre"/></Field>
                        <Field label="Players" target="ed-players">
                            <Choice value=players options=PLAYERS.to_vec() label="Players" id="ed-players"/>
                        </Field>
                    </div>
                </div>
                <Field label="Description" target="ed-description">
                    <textarea
                        class="input"
                        id="ed-description"
                        rows="4"
                        prop:value=move || description.get()
                        on:input=move |ev| description.set(event_target_value(&ev))
                    ></textarea>
                </Field>
                <Field label="Steam app id" target="ed-steam">
                    <TextInput value=steam_app_id id="ed-steam" class="mono" placeholder="Optional" style="width:170px;max-width:100%"/>
                </Field>
                <p class="faint" style="font-size:12px;margin-top:-8px">"Bonfire networking uses it to recognise the game."</p>
                <Field label="Emulator" target="ed-emulator"><Pick id="ed-emulator" value=emulator options=emulator_choices/></Field>
                <Show when=move || emulator.get().is_some_and(|id| id != 0)>
                    <p class="faint" style="font-size:12px;margin-top:-8px">
                        "This game ships only its own files. The default way to start names the game file, and the emulator's command runs it."
                    </p>
                </Show>

                <span class="lab">"Launch setup"</span>
                <Lines rows=launch with_default=true example="bin/game.exe" add="Add a way to start"/>
                <span class="lab">"Runs once per PC before the first start"</span>
                <Lines rows=prerequisites example="redist/vc_redist.x64.exe" add="Add an installer"/>

                <span class="lab">"Versions"</span>
                <div class="card tblw">
                    <table class="tbl" style="min-width:0">
                        <thead>
                            <tr>
                                <th>"Version"</th>
                                <th>"Added"</th>
                                <th class="num">"Update size"</th>
                                <th>"Note for players"</th>
                            </tr>
                        </thead>
                        <tbody>
                            {versions.with_value(|versions| {
                                versions
                                    .iter()
                                    .enumerate()
                                    .map(|(index, (version, name, note))| {
                                        view! {
                                            <tr>
                                                <td>
                                                    <span class="hrow" style="flex-wrap:nowrap">
                                                        <TextInput value=*name label="Version name" class="mono" style="width:110px"/>
                                                        {(index == 0).then(|| view! { <span class="badge ok">"Newest"</span> })}
                                                    </span>
                                                </td>
                                                <td class="muted">{when(version.added_at)}</td>
                                                <td class="num">{version.update_bytes.map(size).unwrap_or_else(|| "—".into())}</td>
                                                <td><TextInput value=*note label="Note for players" style="width:100%;min-width:150px"/></td>
                                            </tr>
                                        }
                                    })
                                    .collect_view()
                            })}
                        </tbody>
                    </table>
                </div>
                <span class="faint" style="font-size:12px">
                    "To publish a newer version, copy the new build into a games folder. It shows up in Metadata review, where you add it as an update. Bonfire then downloads only what changed."
                </span>
                <ErrorNote message=error/>
            </div>
            <footer>
                <button type="button" class="btn" on:click=move |_| on_close.run(())>"Cancel"</button>
                <button class="btn pri" disabled=move || saving.get()>"Save"</button>
            </footer>
        </form>
    }
}

/// Exactly one launch line is the default: the first one marked, or simply the first.
fn one_default(lines: &mut [LaunchOption]) {
    let default = lines.iter().position(|line| line.default).unwrap_or(0);
    for (index, line) in lines.iter_mut().enumerate() {
        line.default = index == default;
    }
}

/// The lines of "Launch setup" and of what runs before the first start: a name, a path inside
/// the game's folder and arguments. Launch lines also say which of them is the default.
#[component]
fn Lines(rows: RwSignal<Vec<LaunchOption>>, #[prop(optional)] with_default: bool, example: &'static str, add: &'static str) -> impl IntoView {
    // A field reads and writes its place in the list, so the list is always what gets saved.
    let field = move |index: usize,
                      label: &'static str,
                      hint: &'static str,
                      style: &'static str,
                      read: fn(&LaunchOption) -> &String,
                      write: fn(&mut LaunchOption) -> &mut String| {
        view! {
            <input
                class="input"
                class:mono={ label != "Name" }
                aria-label=format!("{label} of line {}", index + 1)
                placeholder=hint
                style=style
                autocomplete="off"
                spellcheck="false"
                prop:value=move || rows.with(|rows| rows.get(index).map(|line| read(line).clone()).unwrap_or_default())
                on:input=move |ev| {
                    rows.update(|rows| {
                        if let Some(line) = rows.get_mut(index) {
                            *write(line) = event_target_value(&ev);
                        }
                    })
                }
            />
        }
    };
    let make_default = move |index: usize| rows.update(|rows| rows.iter_mut().enumerate().for_each(|(other, line)| line.default = other == index));
    let remove = move |index: usize| {
        rows.update(|rows| {
            if index < rows.len() {
                rows.remove(index);
            }
            if with_default {
                one_default(rows);
            }
        })
    };
    let add_line = move |_| {
        rows.update(|rows| {
            rows.push(LaunchOption { name: String::new(), path: String::new(), args: String::new(), default: false });
            if with_default {
                one_default(rows);
            }
        })
    };
    view! {
        <div class="card rows">
            // Lines are told apart by their place, so typing in one never redraws it.
            <For
                each=move || 0..rows.with(Vec::len)
                key=|index| *index
                children=move |index| {
                    let default = with_default
                        .then(|| {
                            let is_default = Memo::new(move |_| rows.with(|rows| rows.get(index).is_some_and(|line| line.default)));
                            move || {
                                if is_default.get() {
                                    view! { <span class="badge ok">"Default"</span> }.into_any()
                                } else {
                                    view! { <button type="button" class="btn sm ghost" on:click=move |_| make_default(index)>"Make default"</button> }
                                        .into_any()
                                }
                            }
                        });
                    view! {
                        <div class="row" style="min-height:44px;flex-wrap:wrap">
                            {default}
                            {field(index, "Name", "Name", "flex:1 1 110px", |line| &line.name, |line| &mut line.name)}
                            {field(index, "Path", example, "flex:2 1 170px", |line| &line.path, |line| &mut line.path)}
                            {field(index, "Arguments", "Arguments", "flex:1 1 110px", |line| &line.args, |line| &mut line.args)}
                            <button type="button" class="ib" aria-label=format!("Remove line {}", index + 1) on:click=move |_| remove(index)>
                                <Icon name="trash" size=14/>
                            </button>
                        </div>
                    }
                }
            />
            <div class="row" style="min-height:40px">
                <button type="button" class="btn sm" on:click=add_line><Icon name="plus" size=13/>{add}</button>
            </div>
        </div>
    }
}
