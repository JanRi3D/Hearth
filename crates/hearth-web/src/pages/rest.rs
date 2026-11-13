//! Rest storage: the space people's rested saves take on this Hearth.

use hearth_api::{
    saves::Storage,
    settings::{SettingsPatch, SettingsView},
};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api,
    form::QUOTA_CHOICES,
    ui::{Avatar, Choice, size, toast_err, when},
};

#[component]
pub fn Rest() -> impl IntoView {
    let storage = RwSignal::new(None::<Storage>);
    let quota = RwSignal::new(5u32);
    let load = move || {
        spawn_local(async move {
            match api::get::<Storage>("/admin/storage").await {
                Ok(found) => storage.set(Some(found)),
                Err(err) => toast_err("Rest storage could not be loaded", &err),
            }
        });
    };
    load();
    spawn_local(async move {
        if let Ok(view) = api::get::<SettingsView>("/admin/settings").await {
            quota.set(view.settings.save_quota_gb);
        }
    });
    let save = Callback::new(move |_: ()| {
        let patch = SettingsPatch { save_quota_gb: Some(quota.get_untracked()), ..Default::default() };
        spawn_local(async move {
            match api::patch::<SettingsView>("/admin/settings", &patch).await {
                Ok(_) => load(),
                Err(err) => toast_err("Not saved", &err),
            }
        });
    });

    let sub = move || {
        storage.with(|storage| {
            storage.as_ref().map(|storage| {
                let accounts = storage.accounts.len();
                format!(
                    "{accounts} {} · {} set aside · {} used",
                    if accounts == 1 { "account" } else { "accounts" },
                    size(storage.quota_bytes * accounts as u64),
                    size(storage.accounts.iter().map(|account| account.used_bytes).sum())
                )
            })
        })
    };
    let rows = move || {
        storage.get().map(|storage| {
            let quota = storage.quota_bytes;
            let rows = storage.accounts.into_iter().map(move |account| {
                let share = account.used_bytes as f64 / quota.max(1) as f64;
                let bar = format!("width:{:.1}%;background:var(--{})", share.min(1.0) * 100.0, if share > 0.9 { "warn" } else { "ember" });
                view! {
                    <tr>
                        <td>
                            <span class="hrow" style="flex-wrap:nowrap">
                                <Avatar name=account.username.clone() size=24/>
                                <b style="font-weight:500">{account.username}</b>
                            </span>
                        </td>
                        <td>
                            <div class="hrow" style="flex-wrap:nowrap">
                                <div class="prog" style="flex:1"><i style=bar></i></div>
                                <span class="mono muted" style="font-size:12px;width:130px;text-align:right;flex:none">
                                    {format!("{} of {}", size(account.used_bytes), size(quota))}
                                </span>
                            </div>
                        </td>
                        <td class="num">{account.games}</td>
                        <td class="muted">{account.last_rested_at.map_or("Never".to_string(), when)}</td>
                    </tr>
                }
            });
            rows.collect_view()
        })
    };

    view! {
        <div class="view">
            <div class="ph">
                <div>
                    <span class="lab">"Save storage"</span>
                    <h1 class="pt">"Rest storage"</h1>
                    <p class="sub">{sub}</p>
                </div>
                <div class="tools">
                    <span class="muted">"Space per person"</span>
                    <Choice value=quota options=QUOTA_CHOICES.to_vec() label="Space per person" on_commit=save/>
                </div>
            </div>
            <div class="card tblw">
                <table class="tbl">
                    <thead>
                        <tr>
                            <th>"Person"</th>
                            <th style="width:44%">"Used"</th>
                            <th class="num">"Games"</th>
                            <th>"Last rested"</th>
                        </tr>
                    </thead>
                    <tbody>{rows}</tbody>
                </table>
            </div>
            <p class="faint" style="font-size:12px">"Saves of removed accounts are kept for 30 days."</p>
        </div>
    }
}
