//! The first page an admin sees: how the Hearth is doing right now.

use hearth_api::admin::Overview as Data;
use leptos::prelude::*;

use super::library::{RescanButton, watch_scan};
use crate::{
    App,
    ui::{Avatar, duration, size, size_parts, toast_err, when},
};

/// People online in each of the last 24 hours, oldest first. The last value is now.
#[component]
fn Chart(values: Vec<u32>) -> impl IntoView {
    // The drawing is 600 by 170. The plot inside it runs from X0 to X1 and from Y1 up to Y0.
    const H: f64 = 170.0;
    const X0: f64 = 30.0;
    const X1: f64 = 590.0;
    const Y0: f64 = 14.0;
    const Y1: f64 = 136.0;
    const MONO: &str = "ui-monospace,Consolas,monospace";
    if values.len() < 2 {
        return view! { <div class="empty">"Nothing recorded yet."</div> }.into_any();
    }
    let last = values.len() - 1;
    let (peak, now) = (values.iter().copied().max().unwrap_or(0), values[last]);
    // The axis ends on an even number, at least 4, so the line halfway is a whole number too.
    let top = peak.max(4).next_multiple_of(2);
    let x = move |index: usize| X0 + (X1 - X0) * index as f64 / last as f64;
    let y = move |value: u32| Y1 - (Y1 - Y0) * f64::from(value) / f64::from(top);
    let points = values.iter().enumerate().map(|(index, value)| format!("{:.1},{:.1}", x(index), y(*value))).collect::<Vec<_>>().join(" ");
    let hour = js_sys::Date::new_0().get_hours() as usize;
    let grid = [0, top / 2, top].into_iter().map(|value| {
        let at = y(value);
        view! {
            <line
                x1=X0.to_string()
                x2=X1.to_string()
                y1=at.to_string()
                y2=at.to_string()
                stroke="rgba(255,255,255,.09)"
                stroke-dasharray={ if value == 0 { "0" } else { "3 4" } }
            ></line>
            <text x={ (X0 - 8.0).to_string() } y={ (at + 4.0).to_string() } text-anchor="end" font-size="11" fill="var(--fg-3)" font-family=MONO>
                {value}
            </text>
        }
    });
    // A clock time under every sixth hour, and "now" under the last one.
    let labels = (0..=last).filter(|index| index % 6 == 0 || *index == last).map(|index| {
        let (anchor, label) = match index {
            _ if index == last => ("end", "now".to_string()),
            _ => (if index == 0 { "start" } else { "middle" }, format!("{:02}:00", (hour + 24 - (last - index) % 24) % 24)),
        };
        view! {
            <text x=x(index).to_string() y={ (H - 10.0).to_string() } text-anchor=anchor font-size="11" fill="var(--fg-3)" font-family=MONO>
                {label}
            </text>
        }
    });
    view! {
        <svg class="chart" viewBox=format!("0 0 600 {H}") role="img" aria-label=format!("People online over the last 24 hours: a peak of {peak}, {now} now")>
            {grid.collect_view()}
            <polygon points=format!("{X0},{Y1} {points} {X1},{Y1}") fill="var(--ember-soft)"></polygon>
            <polyline points=points fill="none" stroke="var(--ember)" stroke-width="1.75" stroke-linejoin="round"></polyline>
            <circle cx=X1.to_string() cy=y(now).to_string() r="4" fill="var(--ember)" stroke="#000" stroke-width="2"></circle>
            {labels.collect_view()}
        </svg>
    }
        .into_any()
}

#[component]
pub fn Overview() -> impl IntoView {
    let app = expect_context::<App>();
    let data = RwSignal::new(None::<Data>);
    let watch = watch_scan(move || async move {
        match app.load_overview().await {
            Ok(found) => {
                let scan = found.library.scan.clone();
                data.set(Some(found));
                Some(scan)
            }
            Err(err) => {
                toast_err("The overview could not be loaded", &err);
                None
            }
        }
    });

    view! {
        <div class="view">
            <div class="ph">
                <div>
                    <span class="lab">
                        {move || data.get().map(|data| format!("hearth {} · up for {}", data.version, duration(data.uptime_seconds)))}
                    </span>
                    <h1 class="pt">"Overview"</h1>
                </div>
                <div class="tools"><RescanButton watch=watch idle="Rescan library" unit=" files"/></div>
            </div>
            {move || {
                data.get()
                    .map(|data| {
                        let library = data.library;
                        let place = match library.folders.as_slice() {
                            [folder] => folder.clone(),
                            folders => format!("{} folders", folders.len()),
                        };
                        let (rest_used, rest_unit) = size_parts(data.rest_used_bytes);
                        let scanned = match library.scan.last_finished_at {
                            Some(at) => format!("Last full scan {}.", when(at)),
                            None => "No full scan yet.".to_string(),
                        };
                        let matching = data.services.iter().any(|service| service.name == "Metadata" && service.ok);
                        view! {
                            <div class="kpis">
                                <div class="kpi">
                                    <span class="lab">"Library"</span>
                                    <b>{library.games}<small>{if library.games == 1 { "game" } else { "games" }}</small></b>
                                    <span>{format!("{} in {place}", size(library.bytes))}</span>
                                </div>
                                <div class="kpi">
                                    <span class="lab">"People"</span>
                                    <b>{data.accounts}<small>{if data.accounts == 1 { "account" } else { "accounts" }}</small></b>
                                    <span>{format!("{} online now", data.online_now)}</span>
                                </div>
                                <div class="kpi">
                                    <span class="lab">"Rest storage"</span>
                                    <b>{rest_used}<small>{rest_unit}</small></b>
                                    <span>{format!("of {} set aside", size(data.rest_quota_bytes))}</span>
                                </div>
                                <div class="kpi">
                                    <span class="lab">"Lobbies"</span>
                                    <b>{data.lobbies}<small>"open"</small></b>
                                    <span>{format!("{} carried by the relay", data.relayed_lobbies)}</span>
                                </div>
                            </div>
                            <div class="cols">
                                <div class="card">
                                    <header><h3>"People online"</h3><span class="muted">"last 24 hours"</span></header>
                                    <div class="pad"><Chart values=data.online_24h/></div>
                                </div>
                                <div class="card">
                                    <header><h3>"Playing now"</h3><span class="muted mono">{data.playing_now.len()}</span></header>
                                    <div class="rows">
                                        {if data.playing_now.is_empty() {
                                            view! { <div class="empty">"Nobody is playing."</div> }.into_any()
                                        } else {
                                            data.playing_now
                                                .into_iter()
                                                .map(|player| {
                                                    view! {
                                                        <div class="row">
                                                            <Avatar name=player.username.clone()/>
                                                            <div class="col">
                                                                <span class="t">{player.username}</span>
                                                                <span class="d ell">{player.game_title}</span>
                                                            </div>
                                                            <span class="mono faint" style="font-size:12px">{format!("since {}", when(player.since))}</span>
                                                        </div>
                                                    }
                                                })
                                                .collect_view()
                                                .into_any()
                                        }}
                                    </div>
                                </div>
                            </div>
                            <div class="card">
                                <header style="flex-wrap:wrap">
                                    <h3>{if library.folders.len() == 1 { "Library folder" } else { "Library folders" }}</h3>
                                    <span class="mono muted" style="overflow-wrap:anywhere">{library.folders.join(", ")}</span>
                                    <span class="sp"></span>
                                    {if library.review > 0 {
                                        view! {
                                            <span class="badge warn">
                                                {format!("{} {} a match", library.review, if library.review == 1 { "file needs" } else { "files need" })}
                                            </span>
                                            <button class="btn sm" on:click=move |_| app.go("/admin/review")>"Review"</button>
                                        }
                                            .into_any()
                                    } else {
                                        view! { <span class="badge ok">"Everything matched"</span> }.into_any()
                                    }}
                                </header>
                                <div class="pad col">
                                    <span class="muted">
                                        {format!(
                                            "{} {scanned} {}",
                                            if library.watching { "Watching for changes." } else { "Not watching." },
                                            if matching {
                                                format!("New files are matched against IGDB, and anything at or above {:.2} is added on its own.", library.threshold)
                                            } else {
                                                "New files are added under the name of their file. Add your IGDB keys in Settings to match them.".to_string()
                                            }
                                        )}
                                    </span>
                                </div>
                            </div>
                            <div class="cols even">
                                <div class="card">
                                    <header><h3>"Services"</h3></header>
                                    <div class="svc">
                                        {data
                                            .services
                                            .into_iter()
                                            .map(|service| {
                                                view! {
                                                    <div>
                                                        <i class="dot" class:ok=service.ok></i>
                                                        <div class="col" style="gap:0">
                                                            <span style="font-weight:500">{service.name}</span>
                                                            <span class="faint" style="font-size:12px">{service.detail}</span>
                                                        </div>
                                                    </div>
                                                }
                                            })
                                            .collect_view()}
                                    </div>
                                </div>
                                <div class="card">
                                    <header><h3>"Recent activity"</h3></header>
                                    <div class="rows">
                                        {if data.activity.is_empty() {
                                            view! { <div class="empty">"Nothing has happened yet."</div> }.into_any()
                                        } else {
                                            data.activity
                                                .into_iter()
                                                .map(|entry| {
                                                    view! {
                                                        <div class="row" style="min-height:38px">
                                                            <span class="mono faint" style="font-size:12px;width:44px;flex:none">{when(entry.at)}</span>
                                                            <span>{entry.text}</span>
                                                        </div>
                                                    }
                                                })
                                                .collect_view()
                                                .into_any()
                                        }}
                                    </div>
                                </div>
                            </div>
                        }
                    })
            }}
        </div>
    }
}
