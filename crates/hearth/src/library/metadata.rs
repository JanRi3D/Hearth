//! Details from IGDB and art from SteamGridDB. Both are optional: without keys the library
//! still lists every game under the name of its folder.

use std::{path::Path, sync::Mutex, time::Duration};

use hearth_api::{
    library::{Candidate, Players},
    settings::{MetadataProvider, Settings},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::names::{self, Parsed};
use crate::db::now;

/// Where the providers live. Tests point these at a local stand-in.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub twitch_token: String,
    pub igdb: String,
    pub sgdb: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            twitch_token: "https://id.twitch.tv/oauth2/token".into(),
            igdb: "https://api.igdb.com/v4".into(),
            sgdb: "https://www.steamgriddb.com/api/v2".into(),
        }
    }
}

/// A candidate with everything needed to create the game, so accepting it later in
/// Metadata review does not have to ask the provider again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub candidate: Candidate,
    pub developer: String,
    pub genre: String,
    pub players: Players,
    pub description: String,
}

#[derive(Debug, Default)]
pub struct Art {
    pub cover: Option<String>,
    pub hero: Option<String>,
    pub logo: Option<String>,
}

pub struct Providers {
    http: reqwest::Client,
    endpoints: Endpoints,
    /// The IGDB access token and when it runs out.
    igdb_token: Mutex<Option<(String, i64)>>,
}

type Fetched<T> = Result<T, String>;

fn failed(err: reqwest::Error) -> String {
    // The URL may carry a secret in its query, so it stays out of the message.
    err.without_url().to_string()
}

impl Providers {
    pub fn new(endpoints: Endpoints) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("hearth/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(20))
            .build()
            .expect("the HTTP client needs no more than a TLS provider, which is installed at start");
        Self { http, endpoints, igdb_token: Mutex::new(None) }
    }

    pub fn igdb_ready(settings: &Settings) -> bool {
        settings.igdb_enabled && !settings.igdb_client_id.is_empty() && !settings.igdb_client_secret.is_empty()
    }

    pub fn sgdb_ready(settings: &Settings) -> bool {
        settings.sgdb_enabled && !settings.sgdb_api_key.is_empty()
    }

    /// Candidates for a file, best first. IGDB is asked when it is set up, SteamGridDB otherwise.
    pub async fn search(&self, settings: &Settings, parsed: &Parsed) -> Fetched<Vec<Meta>> {
        let mut found = if Self::igdb_ready(settings) {
            self.igdb_search(settings, parsed).await?
        } else if Self::sgdb_ready(settings) {
            self.sgdb_search(settings, &parsed.title)
                .await?
                .into_iter()
                .map(|(id, title, year)| Meta {
                    candidate: Candidate { confidence: names::confidence(parsed, &title, year), title, year, igdb_id: None, sgdb_id: Some(id) },
                    developer: String::new(),
                    genre: String::new(),
                    players: Players::Single,
                    description: String::new(),
                })
                .collect()
        } else {
            Vec::new()
        };
        found.sort_by(|a, b| b.candidate.confidence.total_cmp(&a.candidate.confidence));

        // Two games with the very same name, a remake for one, cannot be told apart by name.
        // Unless the file names a year, that is for an admin to decide.
        if parsed.year.is_none() {
            let best = found.first().map(|meta| names::normalize(&meta.candidate.title));
            let twins = found.iter().filter(|meta| Some(names::normalize(&meta.candidate.title)) == best).count();
            if twins > 1 {
                for meta in &mut found {
                    meta.candidate.confidence = meta.candidate.confidence.min(0.80);
                }
            }
        }
        found.truncate(5);
        Ok(found)
    }

    // ---------- IGDB ----------

    async fn igdb_access_token(&self, settings: &Settings) -> Fetched<String> {
        if let Some((token, _)) = self.igdb_token.lock().unwrap().clone().filter(|(_, expires)| *expires > now() + 60) {
            return Ok(token);
        }
        let response = self
            .http
            .post(&self.endpoints.twitch_token)
            // In the body, not the address, so the secret never lands in a log.
            .form(&[
                ("client_id", settings.igdb_client_id.as_str()),
                ("client_secret", settings.igdb_client_secret.as_str()),
                ("grant_type", "client_credentials"),
            ])
            .send()
            .await
            .map_err(failed)?;
        if !response.status().is_success() {
            return Err(format!("Twitch refused the IGDB key (status {}). Check the client ID and secret.", response.status().as_u16()));
        }
        let body: Value = response.json().await.map_err(failed)?;
        let token = body["access_token"].as_str().ok_or("Twitch sent no access token.")?.to_string();
        let expires = now() + body["expires_in"].as_i64().unwrap_or(3600);
        *self.igdb_token.lock().unwrap() = Some((token.clone(), expires));
        Ok(token)
    }

    async fn igdb_search(&self, settings: &Settings, parsed: &Parsed) -> Fetched<Vec<Meta>> {
        let token = self.igdb_access_token(settings).await?;
        // The query language quotes strings with double quotes. Titles never need one.
        let title = parsed.title.replace(['"', '\\', ';'], " ");
        let query = format!(
            "search \"{title}\"; fields name,first_release_date,summary,genres.name,game_modes.name,\
             involved_companies.developer,involved_companies.company.name; limit 10;"
        );
        let response = self
            .http
            .post(format!("{}/games", self.endpoints.igdb))
            .header("Client-ID", &settings.igdb_client_id)
            .bearer_auth(token)
            .body(query)
            .send()
            .await
            .map_err(failed)?;
        if !response.status().is_success() {
            if response.status().as_u16() == 401 {
                *self.igdb_token.lock().unwrap() = None;
            }
            return Err(format!("IGDB answered with status {}.", response.status().as_u16()));
        }
        let games: Vec<Value> = response.json().await.map_err(failed)?;
        Ok(games
            .iter()
            .filter_map(|game| {
                let title = game["name"].as_str()?.to_string();
                let year = game["first_release_date"].as_i64().map(year_of);
                let names_of = |field: &str| -> Vec<&str> {
                    game[field].as_array().map(|list| list.iter().filter_map(|item| item["name"].as_str()).collect()).unwrap_or_default()
                };
                let modes = names_of("game_modes");
                let players = if modes.iter().any(|mode| mode.contains("Co-op")) {
                    Players::Coop
                } else if modes.iter().any(|mode| mode.contains("Multiplayer") || mode.contains("MMO")) {
                    Players::Online
                } else {
                    Players::Single
                };
                let developer = game["involved_companies"]
                    .as_array()
                    .and_then(|companies| companies.iter().find(|company| company["developer"].as_bool() == Some(true)))
                    .and_then(|company| company["company"]["name"].as_str())
                    .unwrap_or_default()
                    .to_string();
                Some(Meta {
                    candidate: Candidate {
                        confidence: names::confidence(parsed, &title, year),
                        title,
                        year,
                        igdb_id: game["id"].as_u64(),
                        sgdb_id: None,
                    },
                    developer,
                    genre: names_of("genres").into_iter().take(2).collect::<Vec<_>>().join(", "),
                    players,
                    description: game["summary"].as_str().unwrap_or_default().to_string(),
                })
            })
            .collect())
    }

    // ---------- SteamGridDB ----------

    async fn sgdb_get(&self, settings: &Settings, path: &str) -> Fetched<Value> {
        let response = self.http.get(format!("{}{path}", self.endpoints.sgdb)).bearer_auth(&settings.sgdb_api_key).send().await.map_err(failed)?;
        if !response.status().is_success() {
            return Err(match response.status().as_u16() {
                401 | 403 => "SteamGridDB refused the key. Copy it again from your SteamGridDB profile.".to_string(),
                status => format!("SteamGridDB answered with status {status}."),
            });
        }
        response.json().await.map_err(failed)
    }

    /// (id, name, year) of what SteamGridDB knows under a name.
    async fn sgdb_search(&self, settings: &Settings, title: &str) -> Fetched<Vec<(u64, String, Option<i32>)>> {
        let term: String = url_encode(title);
        let body = self.sgdb_get(settings, &format!("/search/autocomplete/{term}")).await?;
        Ok(body["data"]
            .as_array()
            .map(|games| {
                games
                    .iter()
                    .filter_map(|game| Some((game["id"].as_u64()?, game["name"].as_str()?.to_string(), game["release_date"].as_i64().map(year_of))))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Downloads cover, hero and logo into the art folder and returns their file names.
    pub async fn art(&self, settings: &Settings, game_id: i64, sgdb_id: Option<u64>, title: &str, art_dir: &Path) -> Art {
        let mut art = Art::default();
        if !Self::sgdb_ready(settings) {
            return art;
        }
        let sgdb_id = match sgdb_id {
            Some(id) => Some(id),
            // The details came from IGDB. Take SteamGridDB's entry only when the name is the same game.
            None => self.sgdb_search(settings, title).await.ok().and_then(|found| {
                found.into_iter().find(|(_, name, _)| names::normalize(name) == names::normalize(title)).map(|(id, _, _)| id)
            }),
        };
        let Some(sgdb_id) = sgdb_id else { return art };
        for (kind, path, slot) in [
            ("cover", format!("/grids/game/{sgdb_id}?dimensions=600x900&types=static"), &mut art.cover),
            ("hero", format!("/heroes/game/{sgdb_id}?types=static"), &mut art.hero),
            ("logo", format!("/logos/game/{sgdb_id}?types=static"), &mut art.logo),
        ] {
            let url = match self.sgdb_get(settings, &path).await {
                Ok(body) => body["data"][0]["url"].as_str().map(str::to_string),
                Err(err) => {
                    tracing::warn!(game_id, kind, error = %err, "could not look up art");
                    None
                }
            };
            if let Some(url) = url {
                match self.download_image(&url, &art_dir.join(format!("{game_id}-{kind}"))).await {
                    Ok(file) => *slot = Some(file),
                    Err(err) => tracing::warn!(game_id, kind, error = %err, "could not download art"),
                }
            }
        }
        art
    }

    /// Saves an image next to `stem` with the right extension and returns the file name.
    /// The address comes from a provider's answer, so it is checked before it is fetched.
    async fn download_image(&self, url: &str, stem: &Path) -> Fetched<String> {
        const LIMIT: usize = 20 * 1024 * 1024;
        let parsed = reqwest::Url::parse(url).map_err(|err| err.to_string())?;
        let host = parsed.host_str().unwrap_or_default();
        let ours = reqwest::Url::parse(&self.endpoints.sgdb).ok().and_then(|url| url.host_str().map(str::to_string));
        let trusted = (parsed.scheme() == "https" && (host == "steamgriddb.com" || host.ends_with(".steamgriddb.com"))) || Some(host) == ours.as_deref();
        if !trusted {
            return Err(format!("the image is on {host}, which is not SteamGridDB"));
        }
        let response = self.http.get(parsed).send().await.map_err(failed)?;
        if !response.status().is_success() {
            return Err(format!("status {}", response.status().as_u16()));
        }
        let extension = match response.headers().get("content-type").and_then(|value| value.to_str().ok()).unwrap_or_default() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            other => return Err(format!("not an image the Hearth keeps: {other}")),
        };
        if response.content_length().is_some_and(|length| length as usize > LIMIT) {
            return Err("the image is larger than 20 MB".into());
        }
        let bytes = response.bytes().await.map_err(failed)?;
        if bytes.len() > LIMIT {
            return Err("the image is larger than 20 MB".into());
        }
        let file = stem.with_extension(extension);
        tokio::fs::write(&file, &bytes).await.map_err(|err| err.to_string())?;
        Ok(file.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_string())
    }

    /// For the Test button next to a key: says in words whether the provider accepts it.
    pub async fn test(&self, settings: &Settings, provider: MetadataProvider) -> Fetched<()> {
        match provider {
            MetadataProvider::Igdb => {
                if settings.igdb_client_id.is_empty() || settings.igdb_client_secret.is_empty() {
                    return Err("IGDB needs a client ID and a secret. Paste both first.".into());
                }
                *self.igdb_token.lock().unwrap() = None;
                self.igdb_access_token(settings).await.map(|_| ())
            }
            MetadataProvider::Sgdb => {
                if settings.sgdb_api_key.is_empty() {
                    return Err("SteamGridDB needs one API key. Paste it first.".into());
                }
                self.sgdb_search(settings, "Portal").await.map(|_| ())
            }
        }
    }
}

/// The calendar year a Unix time falls in.
fn year_of(unix: i64) -> i32 {
    time::OffsetDateTime::from_unix_timestamp(unix).map_or(1970, |date| date.year())
}

fn url_encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (byte as char).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_helpers_hold() {
        assert_eq!(year_of(915_148_800), 1999, "the first second of a year belongs to it");
        assert_eq!(year_of(915_148_799), 1998);
        assert_eq!(year_of(1_690_000_000), 2023);
        assert_eq!(url_encode("Baldur's Gate 3"), "Baldur%27s%20Gate%203");
    }
}
