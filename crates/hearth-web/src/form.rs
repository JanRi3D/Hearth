//! The settings as a set of signals, one per option. The setup wizard and the Settings page
//! bind their controls to the same form.

use hearth_api::settings::{JoinMode, Settings, SettingsPatch, TlsMode};
use leptos::prelude::*;

#[derive(Clone, Copy)]
pub struct Form {
    pub name: RwSignal<String>,
    pub address: RwSignal<String>,
    pub port: RwSignal<String>,
    pub tls: RwSignal<TlsMode>,
    pub join: RwSignal<JoinMode>,
    pub games_folders: RwSignal<Vec<String>>,
    pub emulators_folder: RwSignal<String>,
    pub watch_folders: RwSignal<bool>,
    pub match_threshold: RwSignal<f64>,
    pub igdb_enabled: RwSignal<bool>,
    pub igdb_client_id: RwSignal<String>,
    pub igdb_client_secret: RwSignal<String>,
    pub igdb_client_secret_set: RwSignal<bool>,
    pub sgdb_enabled: RwSignal<bool>,
    pub sgdb_api_key: RwSignal<String>,
    pub sgdb_api_key_set: RwSignal<bool>,
    pub relay_enabled: RwSignal<bool>,
    pub relay_port: RwSignal<String>,
    pub networking_default: RwSignal<bool>,
    pub save_quota_gb: RwSignal<u32>,
    pub admin_totp_required: RwSignal<bool>,
    pub session_days: RwSignal<u32>,
    pub lockout_enabled: RwSignal<bool>,
    pub backup_nightly: RwSignal<bool>,
    pub backup_time: RwSignal<String>,
}

pub const TLS_CHOICES: [(TlsMode, &str); 3] = [
    (TlsMode::SelfSigned, "Self-signed certificate"),
    (TlsMode::LetsEncrypt, "Let's Encrypt"),
    (TlsMode::Proxy, "Behind my own proxy"),
];

pub fn tls_note(mode: TlsMode) -> &'static str {
    match mode {
        TlsMode::SelfSigned => "Works right away on your network. Bonfire trusts it after the first connection.",
        TlsMode::LetsEncrypt => "Needs a public domain name that points at this machine, and port 443 from outside has to reach the Hearth's port. Until the certificate arrives the Hearth uses its own. It renews on its own.",
        TlsMode::Proxy => "The Hearth speaks plain HTTP and your proxy adds HTTPS.",
    }
}

pub const JOIN_CHOICES: [(JoinMode, &str); 2] = [(JoinMode::Invite, "Invite only"), (JoinMode::Network, "This network")];
pub const QUOTA_CHOICES: [(u32, &str); 3] = [(2, "2 GB"), (5, "5 GB"), (10, "10 GB")];
pub const THRESHOLD_CHOICES: [(f64, &str); 3] = [(0.85, "0.85"), (0.90, "0.90"), (0.95, "0.95")];
pub const SIGNOUT_CHOICES: [(u32, &str); 4] = [(7, "7 days"), (30, "30 days"), (90, "90 days"), (0, "Never")];

fn port(text: &str, what: &str) -> Result<u16, String> {
    text.trim().parse::<u16>().ok().filter(|port| *port > 0).ok_or(format!("{what} is a number between 1 and 65535."))
}

impl Form {
    pub fn new() -> Self {
        let s = Settings::default();
        Self {
            name: RwSignal::new(s.name),
            address: RwSignal::new(s.address),
            port: RwSignal::new(s.port.to_string()),
            tls: RwSignal::new(s.tls),
            join: RwSignal::new(s.join),
            games_folders: RwSignal::new(s.games_folders),
            emulators_folder: RwSignal::new(s.emulators_folder),
            watch_folders: RwSignal::new(s.watch_folders),
            match_threshold: RwSignal::new(s.match_threshold),
            igdb_enabled: RwSignal::new(s.igdb_enabled),
            igdb_client_id: RwSignal::new(s.igdb_client_id),
            igdb_client_secret: RwSignal::new(String::new()),
            igdb_client_secret_set: RwSignal::new(false),
            sgdb_enabled: RwSignal::new(s.sgdb_enabled),
            sgdb_api_key: RwSignal::new(String::new()),
            sgdb_api_key_set: RwSignal::new(false),
            relay_enabled: RwSignal::new(s.relay_enabled),
            relay_port: RwSignal::new(s.relay_port.to_string()),
            networking_default: RwSignal::new(s.networking_default),
            save_quota_gb: RwSignal::new(s.save_quota_gb),
            admin_totp_required: RwSignal::new(s.admin_totp_required),
            session_days: RwSignal::new(s.session_days),
            lockout_enabled: RwSignal::new(s.lockout_enabled),
            backup_nightly: RwSignal::new(s.backup_nightly),
            backup_time: RwSignal::new(s.backup_time),
        }
    }

    /// Fills the form from what the Hearth has stored. Secrets come back blank by design.
    pub fn load(&self, s: &Settings) {
        self.name.set(s.name.clone());
        self.address.set(s.address.clone());
        self.port.set(s.port.to_string());
        self.tls.set(s.tls);
        self.join.set(s.join);
        self.games_folders.set(s.games_folders.clone());
        self.emulators_folder.set(s.emulators_folder.clone());
        self.watch_folders.set(s.watch_folders);
        self.match_threshold.set(s.match_threshold);
        self.igdb_enabled.set(s.igdb_enabled);
        self.igdb_client_id.set(s.igdb_client_id.clone());
        self.igdb_client_secret.set(String::new());
        self.igdb_client_secret_set.set(s.igdb_client_secret_set);
        self.sgdb_enabled.set(s.sgdb_enabled);
        self.sgdb_api_key.set(String::new());
        self.sgdb_api_key_set.set(s.sgdb_api_key_set);
        self.relay_enabled.set(s.relay_enabled);
        self.relay_port.set(s.relay_port.to_string());
        self.networking_default.set(s.networking_default);
        self.save_quota_gb.set(s.save_quota_gb);
        self.admin_totp_required.set(s.admin_totp_required);
        self.session_days.set(s.session_days);
        self.lockout_enabled.set(s.lockout_enabled);
        self.backup_nightly.set(s.backup_nightly);
        self.backup_time.set(s.backup_time.clone());
    }

    /// Everything, for first-time setup. Fails with a message when a port is not a number.
    pub fn settings(&self) -> Result<Settings, String> {
        Ok(Settings {
            name: self.name.get_untracked(),
            address: self.address.get_untracked(),
            port: port(&self.port.get_untracked(), "The port")?,
            tls: self.tls.get_untracked(),
            join: self.join.get_untracked(),
            games_folders: self.games_folders.get_untracked(),
            emulators_folder: self.emulators_folder.get_untracked(),
            watch_folders: self.watch_folders.get_untracked(),
            match_threshold: self.match_threshold.get_untracked(),
            igdb_enabled: self.igdb_enabled.get_untracked(),
            igdb_client_id: self.igdb_client_id.get_untracked(),
            igdb_client_secret: self.igdb_client_secret.get_untracked(),
            igdb_client_secret_set: false,
            sgdb_enabled: self.sgdb_enabled.get_untracked(),
            sgdb_api_key: self.sgdb_api_key.get_untracked(),
            sgdb_api_key_set: false,
            relay_enabled: self.relay_enabled.get_untracked(),
            relay_port: port(&self.relay_port.get_untracked(), "The relay port")?,
            networking_default: self.networking_default.get_untracked(),
            save_quota_gb: self.save_quota_gb.get_untracked(),
            admin_totp_required: self.admin_totp_required.get_untracked(),
            session_days: self.session_days.get_untracked(),
            lockout_enabled: self.lockout_enabled.get_untracked(),
            backup_nightly: self.backup_nightly.get_untracked(),
            backup_time: self.backup_time.get_untracked(),
        })
    }

    /// The form as a change request. A secret is only part of it when something was typed,
    /// so saving another option never wipes a stored key.
    pub fn patch(&self) -> Result<SettingsPatch, String> {
        let s = self.settings()?;
        let typed = |secret: String| Some(secret).filter(|secret| !secret.trim().is_empty());
        Ok(SettingsPatch {
            name: Some(s.name),
            address: Some(s.address),
            port: Some(s.port),
            tls: Some(s.tls),
            join: Some(s.join),
            games_folders: Some(s.games_folders),
            emulators_folder: Some(s.emulators_folder),
            watch_folders: Some(s.watch_folders),
            match_threshold: Some(s.match_threshold),
            igdb_enabled: Some(s.igdb_enabled),
            igdb_client_id: Some(s.igdb_client_id),
            igdb_client_secret: typed(s.igdb_client_secret),
            sgdb_enabled: Some(s.sgdb_enabled),
            sgdb_api_key: typed(s.sgdb_api_key),
            relay_enabled: Some(s.relay_enabled),
            relay_port: Some(s.relay_port),
            networking_default: Some(s.networking_default),
            save_quota_gb: Some(s.save_quota_gb),
            admin_totp_required: Some(s.admin_totp_required),
            session_days: Some(s.session_days),
            lockout_enabled: Some(s.lockout_enabled),
            backup_nightly: Some(s.backup_nightly),
            backup_time: Some(s.backup_time),
        })
    }
}
