//! The games folders list, the emulators folder and the folder browser. The setup wizard and
//! the Settings page both show it; only the endpoints behind it differ.

use hearth_api::settings::{Browse, FolderScan, FolderScanRequest};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api,
    form::Form,
    ui::{Dialog, ErrorNote, Icon, Srow, TextInput, noop, size, toast},
};

#[derive(Clone, Copy, PartialEq)]
enum Pick {
    Games,
    Emulators,
}

/// `endpoint` is "/setup" during first-time setup and "/admin/folders" afterwards.
#[component]
pub fn FolderEditor(
    form: Form,
    endpoint: &'static str,
    #[prop(into)] data_folder: Signal<String>,
    #[prop(default = noop(), into)] on_commit: Callback<()>,
) -> impl IntoView {
    let new_folder = RwSignal::new(String::new());
    let picking = RwSignal::new(None::<Pick>);
    // The example shows what a path looks like on the machine the Hearth runs on.
    let example = Signal::derive(move || if data_folder.get().contains('\\') { "D:\\Games" } else { "/mnt/nas/games" }.to_string());

    let add = move |path: String| {
        let path = tidy(&path);
        if !is_full_path(&path) {
            return toast(
                "warn",
                "That is not a full path",
                "Start at the top, for example /mnt/nas/games or D:\\Games, or pick the folder with Browse.",
            );
        }
        if form.games_folders.with_untracked(|folders| folders.contains(&path)) {
            return toast("warn", "Already in the list", path);
        }
        form.games_folders.update(|folders| folders.push(path));
        new_folder.set(String::new());
        on_commit.run(());
    };
    let picked = Callback::new(move |path: String| {
        match picking.get_untracked() {
            Some(Pick::Games) => add(path),
            Some(Pick::Emulators) => {
                form.emulators_folder.set(path);
                on_commit.run(());
            }
            None => {}
        }
        picking.set(None);
    });

    view! {
        <div class="card rows">
            <For each=move || form.games_folders.get() key=|path| path.clone() children=move |path| view! { <FolderRow path=path endpoint=endpoint form=form on_commit=on_commit/> }/>
            <div
                class="row"
                style="flex-wrap:wrap"
                // Enter in the path field adds the folder. Without this it would submit the wizard's form.
                on:keydown=move |ev| {
                    if ev.key() == "Enter" && !new_folder.get_untracked().trim().is_empty() {
                        ev.prevent_default();
                        add(new_folder.get_untracked());
                    }
                }
            >
                <TextInput value=new_folder class="mono" placeholder=example label="Path of another games folder" style="flex:1 1 200px"/>
                <button type="button" class="btn sm" on:click=move |_| picking.set(Some(Pick::Games))><Icon name="folder" size=13/>"Browse"</button>
                <button type="button" class="btn sm pri" on:click=move |_| add(new_folder.get_untracked())>"Add folder"</button>
            </div>
        </div>
        <div class="card rows">
            <Srow title="Emulators folder" desc="Emulators are stored here once and shared by the games that need them.">
                <TextInput value=form.emulators_folder class="mono" label="Emulators folder" style="width:240px;max-width:100%" on_commit=on_commit/>
                <button type="button" class="btn sm" on:click=move |_| picking.set(Some(Pick::Emulators))>"Browse"</button>
            </Srow>
            <Srow title="Data folder" desc="Database, backups and rested saves. It is fixed when the Hearth starts, by default a folder next to the program.">
                <span class="mono muted" style="overflow-wrap:anywhere">{move || data_folder.get()}</span>
            </Srow>
        </div>
        <Show when=move || picking.get().is_some()>
            <Browser endpoint=endpoint on_pick=picked on_close=Callback::new(move |_| picking.set(None))/>
        </Show>
    }
}

#[component]
fn FolderRow(path: String, endpoint: &'static str, form: Form, on_commit: Callback<()>) -> impl IntoView {
    let scan = RwSignal::new(None::<FolderScan>);
    {
        let path = path.clone();
        spawn_local(async move {
            if let Ok(found) = api::post::<FolderScan>(&format!("{endpoint}/scan"), &FolderScanRequest { path }).await {
                scan.set(Some(found));
            }
        });
    }
    let remove = {
        let path = path.clone();
        move |_| {
            form.games_folders.update(|folders| folders.retain(|other| *other != path));
            on_commit.run(());
        }
    };
    view! {
        <div class="row">
            <span class="tico" class:ok=move || scan.get().is_some_and(|scan| scan.readable)><Icon name="folder" size=15/></span>
            <div class="col">
                <span class="t mono" style="font-size:12.5px;overflow-wrap:anywhere">{path.clone()}</span>
                <span class="d">{move || scan_text(scan.get())}</span>
            </div>
            {move || {
                scan.get().map(|scan| {
                    if scan.readable {
                        view! { <span class="badge ok hide-s">"Readable"</span> }
                    } else {
                        view! { <span class="badge bad hide-s">"Not readable"</span> }
                    }
                })
            }}
            <button
                type="button"
                class="ib"
                aria-label=format!("Remove {path}")
                disabled=move || form.games_folders.with(|folders| folders.len() < 2)
                on:click=remove
            >
                <Icon name="trash" size=14/>
            </button>
        </div>
    }
}

fn scan_text(scan: Option<FolderScan>) -> String {
    match scan {
        None => "Looking inside.".into(),
        Some(scan) if !scan.exists => "There is no such folder on the machine that runs the Hearth.".into(),
        Some(scan) if !scan.readable => "The Hearth is not allowed to open this folder.".into(),
        Some(scan) if scan.games == 0 => "Nothing in it yet. Games you copy in later are picked up on their own.".into(),
        Some(scan) => format!(
            "{} {} · {}{}",
            scan.games,
            if scan.games == 1 { "game" } else { "games" },
            if scan.complete { "" } else { "more than " },
            size(scan.bytes)
        ),
    }
}

/// Drops trailing separators, but leaves a root like "/" or "D:\" alone.
fn tidy(path: &str) -> String {
    let path = path.trim();
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() || trimmed.ends_with(':') { path.to_string() } else { trimmed.to_string() }
}

fn is_full_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/') || path.starts_with("\\\\") || (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/'))
}

/// Walks the folders of the machine the Hearth runs on. It only ever lists folders.
#[component]
fn Browser(endpoint: &'static str, on_pick: Callback<String>, on_close: Callback<()>) -> impl IntoView {
    let listing = RwSignal::new(None::<Browse>);
    let error = RwSignal::new(String::new());
    let open = move |path: String| {
        spawn_local(async move {
            match api::get::<Browse>(&format!("{endpoint}/browse?path={}", api::encode(&path))).await {
                Ok(found) => {
                    error.set(String::new());
                    listing.set(Some(found));
                }
                Err(err) => error.set(err.message),
            }
        });
    };
    open(String::new());

    let here = move || listing.get().map(|listing| if listing.path.is_empty() { "This machine".to_string() } else { listing.path });
    let entries = move || {
        listing.get().map(|listing| {
            let up = listing.parent.map(|parent| {
                view! {
                    <button type="button" class="row" style="min-height:40px" on:click=move |_| open(parent.clone())>
                        <Icon name="left" size=14/>
                        <span class="muted">"Up one level"</span>
                    </button>
                }
            });
            let dirs = if listing.dirs.is_empty() {
                view! { <div class="empty">"No folders inside."</div> }.into_any()
            } else {
                listing
                    .dirs
                    .into_iter()
                    .map(|dir| {
                        view! {
                            <button type="button" class="row" style="min-height:40px" on:click=move |_| open(dir.path.clone())>
                                <Icon name="folder" size=15/>
                                <span style="flex:1">{dir.name}</span>
                                <span class="faint" style="display:inline-flex"><Icon name="right" size=13/></span>
                            </button>
                        }
                    })
                    .collect_view()
                    .into_any()
            };
            (up, dirs)
        })
    };

    view! {
        <Dialog title="Choose a folder" on_close=on_close over=true>
            <div class="db">
                <div class="code"><span>{here}</span></div>
                <div class="card rows" style="max-height:260px;overflow:auto">{entries}</div>
                <ErrorNote message=error/>
                <span class="faint" style="font-size:12px">
                    "These are folders on the machine that runs the Hearth, not on the PC you are sitting at."
                </span>
            </div>
            <footer>
                <button type="button" class="btn" on:click=move |_| on_close.run(())>"Cancel"</button>
                <button
                    type="button"
                    class="btn pri"
                    disabled=move || listing.get().is_none_or(|listing| listing.path.is_empty())
                    on:click=move |_| {
                        if let Some(listing) = listing.get_untracked() {
                            on_pick.run(listing.path);
                        }
                    }
                >
                    "Use this folder"
                </button>
            </footer>
        </Dialog>
    }
}
