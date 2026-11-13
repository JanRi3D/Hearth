//! The Hearth's web UI: the setup wizard and the admin pages. It is compiled to WebAssembly,
//! embedded in the `hearth` binary and talks to the same `/api/v1` any client uses.

use hearth_api::{
    HearthInfo,
    admin::{LibraryStats, Overview},
    auth::{LoginResponse, Me, Role},
    setup::Handoff,
};
use leptos::{prelude::*, task::spawn_local};

mod api;
mod folders;
mod form;
mod icons;
mod pages;
mod ui;

use pages::{
    login::{Login, NotAdmin, Reset},
    setup::Setup,
    shell::Shell,
};
use ui::{Brand, Toasts};

#[derive(Clone, Copy, PartialEq)]
pub enum Stage {
    Loading,
    Unreachable,
    Setup,
    /// The page behind a password reset link.
    Reset,
    Login,
    NotAdmin,
    Admin,
}

/// What the whole UI shares. Every field is a signal, so the struct is cheap to copy around.
#[derive(Clone, Copy)]
pub struct App {
    pub stage: RwSignal<Stage>,
    pub info: RwSignal<Option<HearthInfo>>,
    pub me: RwSignal<Option<Me>>,
    /// The path in the address bar. Pages are picked from it.
    pub path: RwSignal<String>,
    pub uptime: RwSignal<u64>,
    /// The library at a glance. The sidebar counts the files in Metadata review from it, and
    /// pages name the games folders.
    pub library: RwSignal<LibraryStats>,
    /// Open lobbies, which the sidebar counts next to Bonfire networking.
    pub lobbies: RwSignal<u32>,
}

fn current_path() -> String {
    window().location().pathname().unwrap_or_else(|_| "/".into())
}

impl App {
    /// Works out again what to show: setup, sign-in or the admin pages.
    pub fn restart(self) {
        spawn_local(boot(self));
    }

    pub fn refresh_info(self) {
        spawn_local(async move {
            if let Ok(info) = api::get::<HearthInfo>("/hearth").await {
                self.info.set(Some(info));
            }
        });
    }

    /// Reads the overview, and keeps the parts of it that the frame around the pages shows.
    pub async fn load_overview(self) -> Result<Overview, api::ApiErr> {
        let found = api::get::<Overview>("/admin/overview").await?;
        self.uptime.set(found.uptime_seconds);
        self.library.set(found.library.clone());
        self.lobbies.set(found.lobbies);
        Ok(found)
    }

    /// Shows another page and puts it in the browser's history.
    pub fn go(self, path: &str) {
        if self.path.get_untracked() != path {
            let _ = window().history().and_then(|history| history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(path)));
            self.path.set(path.to_string());
        }
    }

    pub fn name(self) -> String {
        self.info.get().map(|info| info.name).unwrap_or_else(|| "this Hearth".into())
    }

    pub fn sign_out(self) {
        spawn_local(async move {
            let _ = api::post::<()>("/auth/logout", &()).await;
            self.me.set(None);
            self.stage.set(Stage::Login);
        });
    }
}

async fn boot(app: App) {
    // The end of first-time setup sends the browser here with a one-time token, so the admin
    // does not have to type the password again at the new address.
    let location = window().location();
    if let Some(token) = location.hash().ok().and_then(|hash| hash.strip_prefix("#handoff=").map(str::to_string)) {
        let _ = api::post::<LoginResponse>("/auth/handoff", &Handoff { token }).await;
        let _ = window().history().and_then(|history| history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&current_path())));
    }

    let info = match api::get::<HearthInfo>("/hearth").await {
        Ok(info) => info,
        Err(_) => return app.stage.set(Stage::Unreachable),
    };
    let setup_complete = info.setup_complete;
    app.info.set(Some(info));
    if !setup_complete {
        return app.stage.set(Stage::Setup);
    }
    // A password reset link works without being signed in.
    if current_path().starts_with("/reset/") {
        return app.stage.set(Stage::Reset);
    }
    match api::get::<Me>("/me").await {
        Ok(me) => {
            let admin = me.user.role == Role::Admin;
            app.me.set(Some(me));
            if admin && !current_path().starts_with("/admin/") {
                app.go("/admin/overview");
            }
            app.stage.set(if admin { Stage::Admin } else { Stage::NotAdmin });
        }
        Err(err) if err.status == 401 => app.stage.set(Stage::Login),
        Err(_) => app.stage.set(Stage::Unreachable),
    }
}

#[component]
fn Root() -> impl IntoView {
    let app = App {
        stage: RwSignal::new(Stage::Loading),
        info: RwSignal::new(None),
        me: RwSignal::new(None),
        path: RwSignal::new(current_path()),
        uptime: RwSignal::new(0),
        library: RwSignal::new(LibraryStats::default()),
        lobbies: RwSignal::new(0),
    };
    provide_context(app);
    // Back and forward in the browser move between pages without loading anything.
    let _ = window_event_listener(leptos::ev::popstate, move |_| app.path.set(current_path()));
    app.restart();

    view! {
        <div class="stage">
            {move || match app.stage.get() {
                Stage::Loading => view! { <div class="lock"><Brand text="Hearth"/></div> }.into_any(),
                Stage::Unreachable => {
                    view! {
                        <div class="lock">
                            <div class="onb">
                                <div class="hrow"><Brand text="Hearth"/></div>
                                <h1>"The Hearth is not answering"</h1>
                                <p class="muted">"It may be starting up or moving to a new address. Check that it is running, then try again."</p>
                                <div class="hrow" style="justify-content:flex-end"><button class="btn pri" on:click=move |_| app.restart()>"Try again"</button></div>
                            </div>
                        </div>
                    }
                        .into_any()
                }
                Stage::Setup => view! { <Setup/> }.into_any(),
                Stage::Reset => view! { <Reset/> }.into_any(),
                Stage::Login => view! { <Login/> }.into_any(),
                Stage::NotAdmin => view! { <NotAdmin/> }.into_any(),
                Stage::Admin => view! { <Shell/> }.into_any(),
            }}
            <Toasts/>
        </div>
    }
}

#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(Root);
}
