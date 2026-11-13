//! Settings live in one database row. This module loads, checks and changes them.

use std::{
    net::{IpAddr, UdpSocket},
    path::Path,
};

use hearth_api::settings::{Settings, SettingsPatch, TlsMode};
use sqlx::SqlitePool;

use crate::{
    error::{ApiError, ApiResult},
    paths::Paths,
};

pub async fn load(db: &SqlitePool) -> Result<Option<Settings>, sqlx::Error> {
    let json: Option<String> = sqlx::query_scalar("SELECT json FROM settings WHERE id = 1").fetch_optional(db).await?;
    Ok(json.and_then(|json| match serde_json::from_str(&json) {
        Ok(settings) => Some(settings),
        Err(err) => {
            tracing::error!(error = %err, "the stored settings could not be read, starting from defaults");
            None
        }
    }))
}

pub async fn save(db: &SqlitePool, settings: &Settings) -> ApiResult<()> {
    let json = serde_json::to_string(settings)?;
    sqlx::query("INSERT INTO settings (id, json) VALUES (1, ?) ON CONFLICT (id) DO UPDATE SET json = excluded.json")
        .bind(json)
        .execute(db)
        .await?;
    Ok(())
}

/// What a fresh Hearth starts from, before the wizard has asked anything.
pub fn defaults(paths: &Paths) -> Settings {
    Settings {
        name: host_name(),
        address: lan_ip().map(|ip| ip.to_string()).unwrap_or_else(|| "localhost".into()),
        emulators_folder: paths.dir("emulators").display().to_string(),
        ..Settings::default()
    }
}

fn host_name() -> String {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|var| std::env::var(var).ok())
        .map(|name| name.trim().to_lowercase())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "home".into())
}

/// The address this machine uses on its network. Nothing is sent: connecting a UDP socket only
/// asks the system which interface it would use.
pub fn lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    socket.local_addr().ok().map(|addr| addr.ip()).filter(|ip| !ip.is_unspecified())
}

/// The settings as an admin may see them: secrets blanked, with a flag saying whether one is stored.
pub fn public_view(settings: &Settings) -> Settings {
    Settings {
        igdb_client_secret_set: !settings.igdb_client_secret.is_empty(),
        igdb_client_secret: String::new(),
        sgdb_api_key_set: !settings.sgdb_api_key.is_empty(),
        sgdb_api_key: String::new(),
        ..settings.clone()
    }
}

pub fn apply_patch(s: &mut Settings, p: SettingsPatch) {
    macro_rules! take {
        ($($field:ident),*) => {$( if let Some(value) = p.$field { s.$field = value; } )*};
    }
    take!(
        name, address, port, tls, join, games_folders, emulators_folder, watch_folders, match_threshold,
        igdb_enabled, igdb_client_id, igdb_client_secret, sgdb_enabled, sgdb_api_key, relay_enabled, relay_port,
        networking_default, save_quota_gb, admin_totp_required, session_days, lockout_enabled, backup_nightly,
        backup_time
    );
}

/// Tidies the settings in place and refuses values the Hearth could not run with.
pub fn validate(s: &mut Settings) -> ApiResult<()> {
    let bad = |message: &str| Err(ApiError::bad_request(message));

    s.name = s.name.trim().to_string();
    if s.name.is_empty() || s.name.chars().count() > 40 {
        return bad("Give the Hearth a name of up to 40 characters.");
    }
    s.address = s.address.trim().trim_end_matches('.').to_lowercase();
    let host_ok = !s.address.is_empty()
        && s.address.len() <= 253
        && (s.address.parse::<IpAddr>().is_ok() || s.address.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.'));
    if !host_ok {
        return bad("The address is a name or IP without spaces, for example home.lan.");
    }
    if s.port == 0 || s.relay_port == 0 {
        return bad("A port is a number between 1 and 65535.");
    }
    if s.tls == TlsMode::LetsEncrypt && (s.address.parse::<IpAddr>().is_ok() || !s.address.contains('.')) {
        return bad("Let's Encrypt needs a public domain name as the address, for example hearth.example.org.");
    }
    if ![0.85, 0.90, 0.95].contains(&s.match_threshold) {
        return bad("The match threshold is 0.85, 0.90 or 0.95.");
    }
    if ![2, 5, 10].contains(&s.save_quota_gb) {
        return bad("Rest storage per person is 2, 5 or 10 GB.");
    }
    if ![0, 7, 30, 90].contains(&s.session_days) {
        return bad("People are signed out after 7, 30 or 90 days, or never.");
    }
    if !valid_time(&s.backup_time) {
        return bad("The backup time looks like 04:00.");
    }
    s.igdb_client_id = s.igdb_client_id.trim().to_string();
    s.igdb_client_secret = s.igdb_client_secret.trim().to_string();
    s.sgdb_api_key = s.sgdb_api_key.trim().to_string();

    let mut folders: Vec<String> = Vec::new();
    for folder in &s.games_folders {
        let folder = clean_folder(folder)?;
        if !Path::new(&folder).is_dir() {
            return Err(ApiError::bad_request(format!("{folder} is not a folder the Hearth can open.")));
        }
        if !folders.contains(&folder) {
            folders.push(folder);
        }
    }
    s.games_folders = folders;

    s.emulators_folder = clean_folder(&s.emulators_folder)?;
    if std::fs::create_dir_all(&s.emulators_folder).is_err() {
        return Err(ApiError::bad_request(format!("The emulators folder {} could not be created.", s.emulators_folder)));
    }
    Ok(())
}

/// A folder has to be a full path, so it means the same no matter where the Hearth was started.
fn clean_folder(folder: &str) -> ApiResult<String> {
    let folder = folder.trim();
    let path = Path::new(folder);
    if !path.is_absolute() {
        return Err(ApiError::bad_request(format!(
            "\"{folder}\" is not a full path. Start at the top, for example /mnt/nas/games or D:\\Games."
        )));
    }
    // Drop a trailing separator, but keep the root itself ("/" or "D:\").
    let cleaned: std::path::PathBuf = path.components().collect();
    Ok(cleaned.display().to_string())
}

fn valid_time(time: &str) -> bool {
    parse_time(time).is_some()
}

/// "HH:MM" to hour and minute.
pub fn parse_time(time: &str) -> Option<(u8, u8)> {
    let (h, m) = time.split_once(':')?;
    if h.len() != 2 || m.len() != 2 {
        return None;
    }
    let (h, m) = (h.parse::<u8>().ok()?, m.parse::<u8>().ok()?);
    (h < 24 && m < 60).then_some((h, m))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> Settings {
        let dir = std::env::temp_dir();
        Settings {
            address: "home.lan".into(),
            games_folders: vec![dir.display().to_string()],
            emulators_folder: dir.display().to_string(),
            ..Settings::default()
        }
    }

    #[test]
    fn accepts_sound_settings_and_tidies_them() {
        let mut s = valid();
        s.name = "  home ".into();
        s.address = "Home.LAN.".into();
        s.games_folders.push(s.games_folders[0].clone());
        validate(&mut s).unwrap();
        assert_eq!(s.name, "home");
        assert_eq!(s.address, "home.lan");
        assert_eq!(s.games_folders.len(), 1, "a folder listed twice is kept once");
    }

    #[test]
    fn refuses_what_the_hearth_cannot_run_with() {
        let cases: [fn(&mut Settings); 8] = [
            |s| s.name = " ".into(),
            |s| s.address = "http://home.lan".into(),
            |s| s.port = 0,
            |s| s.match_threshold = 0.5,
            |s| s.save_quota_gb = 3,
            |s| s.session_days = 1,
            |s| s.backup_time = "4am".into(),
            |s| s.games_folders = vec!["games".into()],
        ];
        for (i, break_it) in cases.iter().enumerate() {
            let mut s = valid();
            break_it(&mut s);
            assert!(validate(&mut s).is_err(), "case {i} should be refused");
        }
        let mut s = valid();
        s.tls = TlsMode::LetsEncrypt;
        s.address = "192.168.1.20".into();
        assert!(validate(&mut s).is_err(), "Let's Encrypt cannot issue for an IP");
    }

    #[test]
    fn secrets_never_leave_and_patches_touch_only_what_they_name() {
        let mut s = valid();
        apply_patch(&mut s, SettingsPatch { sgdb_api_key: Some("secret".into()), port: Some(7400), ..Default::default() });
        assert_eq!((s.port, s.relay_port), (7400, 7301));
        let view = public_view(&s);
        assert!(view.sgdb_api_key.is_empty() && view.sgdb_api_key_set);
        assert!(!view.igdb_client_secret_set);
    }
}
