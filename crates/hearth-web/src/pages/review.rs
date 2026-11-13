//! Files the Hearth could not place on its own. The admin says what each one is.

use hearth_api::{
    Id,
    library::{Candidate, ReviewAction, ReviewItem, VersionRef},
};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    App, api,
    ui::{Dialog, ErrorNote, Field, Icon, TextInput, size, toast, toast_err},
};

/// Tells the Hearth what a file is: its id, the decision, and what the toast says afterwards.
type Decide = Callback<(Id, ReviewAction, &'static str)>;

#[component]
pub fn Review() -> impl IntoView {
    let app = expect_context::<App>();
    let items = RwSignal::new(None::<Vec<ReviewItem>>);
    // The file that is being added as an update, and the game it belongs to.
    let updating = RwSignal::new(None::<(ReviewItem, VersionRef)>);

    let load = move || {
        spawn_local(async move {
            match api::get::<Vec<ReviewItem>>("/admin/review").await {
                Ok(found) => {
                    app.library.update(|stats| stats.review = found.len() as u32);
                    items.set(Some(found));
                }
                Err(err) => toast_err("The files could not be loaded", &err),
            }
        });
    };
    load();
    let decide: Decide = Callback::new(move |(id, action, done): (Id, ReviewAction, &'static str)| {
        spawn_local(async move {
            match api::post::<()>(&format!("/admin/review/{id}"), &action).await {
                Ok(()) => toast("ok", done, ""),
                Err(err) => toast_err("That did not work", &err),
            }
            load();
        });
    });
    let sub = move || {
        // Not known until the overview has been read.
        let threshold = app.library.with(|stats| stats.threshold);
        let at = if threshold > 0.0 { format!("{threshold:.2}") } else { "the threshold in Settings".to_string() };
        format!("Matches at or above {at} are applied on their own. These fell below.")
    };

    view! {
        <div class="view">
            <div class="ph">
                <div>
                    <span class="lab">"Files the Hearth could not place on its own"</span>
                    <h1 class="pt">"Metadata review"</h1>
                    <p class="sub">{sub}</p>
                </div>
            </div>
            {move || {
                items.get().map(|items| {
                    if items.is_empty() {
                        view! { <div class="card empty">"Every file in your games folders has a match."</div> }.into_any()
                    } else {
                        view! {
                            <div class="card rows">
                                {items.into_iter().map(|item| view! { <Item item=item decide=decide updating=updating/> }).collect_view()}
                            </div>
                        }
                            .into_any()
                    }
                })
            }}
        </div>
        {move || {
            updating
                .get()
                .map(|(item, game)| view! { <UpdateDialog item=item game=game decide=decide on_close=Callback::new(move |_| updating.set(None))/> })
        }}
    }
}

fn name(candidate: &Candidate) -> String {
    match candidate.year {
        Some(year) => format!("{} ({year})", candidate.title),
        None => candidate.title.clone(),
    }
}

#[component]
fn Item(item: ReviewItem, decide: Decide, updating: RwSignal<Option<(ReviewItem, VersionRef)>>) -> impl IntoView {
    let id = item.id;
    let pick = RwSignal::new(0usize);
    let candidates = StoredValue::new(item.candidates.clone());
    let matched = !item.candidates.is_empty();
    let update = item.update_of.clone();
    let is_update = update.is_some();
    let detail = match &update {
        Some(game) => format!("{} · looks like an update for {}", size(item.bytes), game.name),
        None => size(item.bytes),
    };
    let best = move || match candidates.with_value(|list| list.get(pick.get()).cloned()) {
        Some(candidate) => {
            let tone = if candidate.confidence >= 0.85 {
                "ok"
            } else if candidate.confidence >= 0.6 {
                "warn"
            } else {
                "bad"
            };
            view! {
                <span class="ell">{name(&candidate)}</span>
                <span class="mono" style=format!("font-size:12px;color:var(--{tone})")>{format!("{:.2} confidence", candidate.confidence)}</span>
            }
                .into_any()
        }
        None => view! { <span class="faint">"No match found"</span> }.into_any(),
    };
    let choices = (item.candidates.len() > 1).then(|| {
        let choices = item.candidates.iter().enumerate().map(|(index, candidate)| {
            view! {
                <button class="pick" style="padding:5px 10px" aria-pressed=move || (pick.get() == index).to_string() on:click=move |_| pick.set(index)>
                    <span style="flex:1">{name(candidate)}</span>
                    <span class="mono muted">{format!("{:.2}", candidate.confidence)}</span>
                </button>
            }
        });
        view! { <div class="cands">{choices.collect_view()}</div> }
    });
    let file = item.name.clone();
    let add_update = update.map(|game| {
        view! { <button class="btn sm pri" on:click=move |_| updating.set(Some((item.clone(), game.clone())))>"Add as an update"</button> }
    });

    view! {
        <div>
            <div class="row" style="flex-wrap:wrap">
                <span class="tico"><Icon name="box" size=15/></span>
                <div class="col" style="flex:1 1 220px">
                    <span class="t mono" style="font-size:12px;overflow-wrap:anywhere">{file}</span>
                    <span class="d">{detail}</span>
                </div>
                <div class="col" style="gap:0;flex:0 1 230px">{best}</div>
                <div class="hrow">
                    {add_update}
                    {matched
                        .then(|| {
                            let accept = move |_| decide.run((id, ReviewAction::Accept { candidate: pick.get_untracked() }, "Added to the library"));
                            view! { <button class="btn sm" class:pri={ !is_update } on:click=accept>"Accept"</button> }
                        })}
                    <button
                        class="btn sm"
                        class:pri={ !is_update && !matched }
                        class:ghost=matched
                        on:click=move |_| decide.run((id, ReviewAction::AddPlain, "Added to the library"))
                    >
                        "Add without details"
                    </button>
                    <button class="btn sm ghost" on:click=move |_| decide.run((id, ReviewAction::Ignore, "Ignored"))>"Ignore"</button>
                </div>
            </div>
            {choices}
        </div>
    }
}

/// Asks what to call a file that becomes a newer version of a game.
#[component]
fn UpdateDialog(item: ReviewItem, game: VersionRef, decide: Decide, on_close: Callback<()>) -> impl IntoView {
    let version = RwSignal::new(item.version_guess.unwrap_or_default());
    let note = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let (id, game_id) = (item.id, game.id);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let version = version.get_untracked().trim().to_string();
        if version.is_empty() {
            return error.set("Name the version, for example 2.0.16.".into());
        }
        decide.run((id, ReviewAction::Update { game_id, version, note: note.get_untracked() }, "Added as an update"));
        on_close.run(());
    };
    view! {
        <Dialog title=format!("Add an update for {}", game.name) on_close=on_close>
            <form on:submit=submit style="display:contents">
                <div class="db">
                    <div class="code"><span>{item.name}</span></div>
                    <Field label="Version name" target="rv-version"><TextInput value=version id="rv-version" class="mono" placeholder="2.0.16"/></Field>
                    <Field label="Note for players" target="rv-note"><TextInput value=note id="rv-note" placeholder="Optional"/></Field>
                    <span class="faint" style="font-size:12px">"Players can kindle the update once the Hearth has prepared its files."</span>
                    <ErrorNote message=error/>
                </div>
                <footer>
                    <button type="button" class="btn" on:click=move |_| on_close.run(())>"Cancel"</button>
                    <button class="btn pri">"Add as an update"</button>
                </footer>
            </form>
        </Dialog>
    }
}
