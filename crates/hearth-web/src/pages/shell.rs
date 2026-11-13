//! The frame around every admin page: navigation on the left, a bar on top, the page below.

use leptos::{prelude::*, task::spawn_local};

use super::{
    emulators::Emulators, events::Events, firelink::Firelink, library::Library, networking::Networking, news::News, overview::Overview,
    people::People, rest::Rest, review::Review, settings::Settings,
};
use crate::{
    App,
    ui::{Avatar, Icon, duration},
};

/// One page of the admin: (path key, icon, label).
type Page = (&'static str, &'static str, &'static str);

/// Sections of the sidebar: a heading, then its pages.
const NAV: [(&str, &[Page]); 3] = [
    (
        "Hearth",
        &[
            ("overview", "activity", "Overview"),
            ("library", "grid", "Library"),
            ("emulators", "chip", "Emulators"),
            ("review", "tag", "Metadata review"),
            ("people", "users", "People and invites"),
            ("news", "news", "News"),
            ("events", "calendar", "Events"),
        ],
    ),
    ("Multiplayer", &[("networking", "plug", "Bonfire networking"), ("firelink", "globe", "Firelink")]),
    ("System", &[("rest", "cloud", "Rest storage"), ("settings", "sliders", "Settings")]),
];

fn title(key: &str) -> &'static str {
    NAV.iter().flat_map(|(_, pages)| pages.iter()).find(|(page, _, _)| *page == key).map_or("Overview", |(_, _, label)| label)
}

#[component]
pub fn Shell() -> impl IntoView {
    let app = expect_context::<App>();
    let side = RwSignal::new(window().inner_width().ok().and_then(|width| width.as_f64()).is_none_or(|width| width > 780.0));
    let menu = RwSignal::new(false);
    let page = Memo::new(move |_| app.path.get().strip_prefix("/admin/").unwrap_or("overview").trim_end_matches('/').to_string());
    let go = move |key: &'static str| {
        app.go(&format!("/admin/{key}"));
        // On a narrow window the sidebar is a drawer, and picking a page closes it.
        if window().inner_width().ok().and_then(|width| width.as_f64()).is_some_and(|width| width <= 780.0) {
            side.set(false);
        }
    };
    let version = move || app.info.get().map(|info| format!("hearth {}", info.version)).unwrap_or_default();
    // The sidebar counts the files in Metadata review and the open lobbies. The Overview page
    // reads the same numbers when it opens, so only the other pages need them read here.
    if page.get_untracked() != "overview" {
        spawn_local(async move {
            let _ = app.load_overview().await;
        });
    }

    view! {
        <section class="win admin" class:no-side=move || !side.get() aria-label="Hearth admin">
            <aside class="side" hidden=move || !side.get()>
                <div class="hs" style="cursor:default">
                    <span class="hmark"><Icon name="flame" size=14 stroke=2.0/></span>
                    <span class="col" style="gap:0;flex:1">
                        <b style="font-weight:600">{move || app.name()}</b>
                        <span class="faint" style="font-size:11.5px">"Hearth admin"</span>
                    </span>
                    <span class="badge ok"><i class="dot ok"></i>"Up"</span>
                </div>
                <nav class="glist" style="padding-top:4px" aria-label="Hearth admin">
                    {NAV
                        .iter()
                        .map(|(heading, pages)| {
                            view! {
                                <div class="navh">{*heading}</div>
                                {pages
                                    .iter()
                                    .map(|(key, icon, label)| {
                                        view! {
                                            <button
                                                class="ni"
                                                aria-current=move || (page.get() == *key).then_some("page")
                                                on:click=move |_| go(key)
                                            >
                                                <Icon name=icon/>
                                                <span class="ell">{*label}</span>
                                                <span class="end">
                                                    {move || {
                                                        let count = match *key {
                                                            "review" => app.library.with(|stats| stats.review),
                                                            "networking" => app.lobbies.get(),
                                                            _ => 0,
                                                        };
                                                        (count > 0).then_some(count)
                                                    }}
                                                </span>
                                            </button>
                                        }
                                    })
                                    .collect_view()}
                            }
                        })
                        .collect_view()}
                </nav>
                <div class="sfoot">
                    <div class="faint mono" style="font-size:11px;padding:4px 6px">
                        {move || format!("{} · up for {}", version(), duration(app.uptime.get()))}
                    </div>
                </div>
            </aside>
            <header class="top">
                <button class="ib" aria-label="Toggle sidebar" aria-pressed=move || side.get().to_string() on:click=move |_| side.update(|open| *open = !*open)>
                    <Icon name="panelL"/>
                </button>
                <div class="crumbs">
                    <span class="hide-s">"Hearth admin"</span>
                    <span class="faint hide-s">"/"</span>
                    <b>{move || title(&page.get())}</b>
                </div>
                <span class="sp"></span>
                <div class="rel">
                    <button class="pbtn" aria-haspopup="menu" aria-expanded=move || menu.get().to_string() on:click=move |_| menu.update(|open| *open = !*open)>
                        {move || app.me.get().map(|me| view! { <Avatar name=me.user.username.clone() size=26/><span class="ell hide-s">{me.user.username}</span> })}
                        <Icon name="down" size=13/>
                    </button>
                    <Show when=move || menu.get()>
                        <div class="menu right" role="menu">
                            <button on:click=move |_| app.sign_out()><Icon name="out" size=15/>"Sign out"</button>
                        </div>
                    </Show>
                </div>
            </header>
            <main class="main" on:click=move |_| menu.set(false)>
                {move || match page.get().as_str() {
                    "library" => view! { <Library/> }.into_any(),
                    "emulators" => view! { <Emulators/> }.into_any(),
                    "news" => view! { <News/> }.into_any(),
                    "events" => view! { <Events/> }.into_any(),
                    "firelink" => view! { <Firelink/> }.into_any(),
                    "rest" => view! { <Rest/> }.into_any(),
                    "review" => view! { <Review/> }.into_any(),
                    "people" => view! { <People/> }.into_any(),
                    "networking" => view! { <Networking/> }.into_any(),
                    "settings" => view! { <Settings/> }.into_any(),
                    _ => view! { <Overview/> }.into_any(),
                }}
            </main>
            <Show when=move || side.get()>
                <button class="drawer-scrim" aria-label="Close sidebar" on:click=move |_| side.set(false)></button>
            </Show>
        </section>
    }
}
