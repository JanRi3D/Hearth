//! Self-update: looks at the newest release of Hearth, downloads the program for this
//! platform, checks it against the release's checksums and puts it in place.
//!
//! A release carries one file per platform, named `hearth-<target>` (plus `.exe` on
//! Windows), and a `SHA256SUMS` file listing the SHA-256 of each.

use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use sha2::{Digest, Sha256};

use crate::tls::hex;

pub const REPOSITORY: &str = "JanRi3D/Hearth";
pub const DEFAULT_API: &str = "https://api.github.com";
const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// No release of Hearth comes near this. It keeps a wrong address from filling the disk.
const LIMIT: usize = 500 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    /// (file name, where to download it).
    pub assets: Vec<(String, String)>,
}

/// The file this platform needs from a release.
pub fn asset_name() -> String {
    format!("hearth-{}{}", env!("HEARTH_TARGET"), std::env::consts::EXE_SUFFIX)
}

/// Compares versions number by number: 0.10.0 is newer than 0.9.3.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let numbers = |version: &str| version.trim_start_matches('v').split(['.', '-', '+']).map(|part| part.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    numbers(candidate) > numbers(current)
}

pub fn newer_than_running(version: &str) -> bool {
    is_newer(version, CURRENT)
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder().user_agent(concat!("hearth/", env!("CARGO_PKG_VERSION"))).timeout(Duration::from_secs(600)).build().map_err(|err| err.to_string())
}

/// Asks for the newest release.
pub async fn latest(api: &str) -> Result<Release, String> {
    let response = client()?.get(format!("{api}/repos/{REPOSITORY}/releases/latest")).send().await.map_err(|err| err.without_url().to_string())?;
    if response.status().as_u16() == 404 {
        return Err("No release of Hearth has been published yet.".into());
    }
    if !response.status().is_success() {
        return Err(format!("The release server answered with status {}.", response.status().as_u16()));
    }
    let body: serde_json::Value = response.json().await.map_err(|err| err.without_url().to_string())?;
    let version = body["tag_name"].as_str().ok_or("The release has no version.")?.trim_start_matches('v').to_string();
    let assets = body["assets"]
        .as_array()
        .map(|assets| assets.iter().filter_map(|asset| Some((asset["name"].as_str()?.to_string(), asset["browser_download_url"].as_str()?.to_string()))).collect())
        .unwrap_or_default();
    Ok(Release { version, assets })
}

async fn fetch(api: &str, url: &str) -> Result<Vec<u8>, String> {
    // Downloads come from where the release was announced, or from GitHub's own file hosts.
    let parsed = reqwest::Url::parse(url).map_err(|err| err.to_string())?;
    let host = parsed.host_str().unwrap_or_default().to_string();
    let announced = reqwest::Url::parse(api).ok().and_then(|api| api.host_str().map(str::to_string));
    let github = parsed.scheme() == "https" && (host == "github.com" || host.ends_with(".github.com") || host.ends_with(".githubusercontent.com"));
    if !github && Some(&host) != announced.as_ref() {
        return Err(format!("The release points at {host}, which is not where Hearth is published."));
    }
    let response = client()?.get(parsed).send().await.map_err(|err| err.without_url().to_string())?;
    if !response.status().is_success() {
        return Err(format!("The download answered with status {}.", response.status().as_u16()));
    }
    let bytes = response.bytes().await.map_err(|err| err.without_url().to_string())?;
    if bytes.len() > LIMIT {
        return Err("The download is larger than any release of Hearth.".into());
    }
    Ok(bytes.to_vec())
}

/// Downloads the program of a release for this platform into `into` and returns its path.
/// Fails unless the file matches the checksum the release lists for it.
pub async fn download(api: &str, release: &Release, into: &Path) -> Result<PathBuf, String> {
    let name = asset_name();
    let url_of = |wanted: &str| release.assets.iter().find(|(name, _)| name == wanted).map(|(_, url)| url.clone());
    let program_url = url_of(&name).ok_or_else(|| format!("Release {} has no build for this platform ({name}).", release.version))?;
    let sums_url = url_of("SHA256SUMS").ok_or("The release has no SHA256SUMS file to check the download against.")?;

    let sums = String::from_utf8(fetch(api, &sums_url).await?).map_err(|_| "SHA256SUMS is not text.".to_string())?;
    let expected = sums
        .lines()
        .filter_map(|line| line.split_once(char::is_whitespace))
        .find(|(_, file)| file.trim().trim_start_matches('*') == name)
        .map(|(sum, _)| sum.trim().to_lowercase())
        .ok_or_else(|| format!("SHA256SUMS does not list {name}."))?;
    let program = fetch(api, &program_url).await?;
    if hex(&Sha256::digest(&program)) != expected {
        return Err("The download does not match its checksum. Nothing was changed. Try again later.".into());
    }
    let file = into.join(format!("{name}.new"));
    tokio::fs::write(&file, &program).await.map_err(|err| format!("Could not write {}: {err}", file.display()))?;
    Ok(file)
}

/// Puts a downloaded program in place of the one that is running. The running process
/// keeps going with the old one until it restarts.
pub fn install(new: &Path) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let old = exe.with_extension("old");
    let _ = std::fs::remove_file(&old);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(new, std::fs::Permissions::from_mode(0o755))?;
    }
    // A running program cannot be overwritten on Windows, but it can be renamed.
    std::fs::rename(&exe, &old)?;
    if let Err(err) = std::fs::rename(new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(err);
    }
    Ok(())
}

/// Removes what an earlier update left behind.
pub fn clean_up() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_extension("old"));
    }
}

/// Starts the program again with the same arguments, in place of this process.
pub fn restart() -> io::Error {
    let (exe, args) = match std::env::current_exe() {
        Ok(exe) => (exe, std::env::args_os().skip(1)),
        Err(err) => return err,
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        std::process::Command::new(exe).args(args).exec()
    }
    #[cfg(not(unix))]
    {
        match std::process::Command::new(exe).args(args).spawn() {
            Ok(_) => std::process::exit(0),
            Err(err) => err,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert!(is_newer("0.4.2", "0.4.1") && is_newer("v0.10.0", "0.9.3") && is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.4.1", "0.4.1") && !is_newer("0.4.0", "0.4.1") && !is_newer("garbage", "0.1.0"));
        assert!(asset_name().starts_with("hearth-") && asset_name().contains(std::env::consts::ARCH));
    }
}
