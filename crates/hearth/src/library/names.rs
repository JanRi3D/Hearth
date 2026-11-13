//! Reading a game's name out of a file or folder name, and telling how alike two names are.
//! Everything here is plain text work, with no disk or network involved.

/// What a file or folder name says about its content.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Parsed {
    pub title: String,
    pub year: Option<i32>,
    pub version: Option<String>,
    /// The name says patch, update or hotfix.
    pub patch: bool,
}

/// Words that describe the packaging, not the game.
const NOISE: [&str; 24] = [
    "win", "windows", "win32", "win64", "x64", "x86", "64bit", "32bit", "linux", "mac", "macos", "osx", "gog", "setup",
    "installer", "install", "portable", "repack", "drmfree", "steamrip", "eng", "multi", "retail", "final",
];
const PATCH_WORDS: [&str; 4] = ["patch", "update", "hotfix", "upd"];

/// "tunic_win_1.0.2.zip" gives title "Tunic", version "1.0.2".
/// "Tyrian 2000 (1999).zip" gives title "Tyrian 2000", year 1999.
/// "bg3_hotfix_31.zip" gives title "bg3", version "hotfix 31", and says it is a patch.
pub fn parse(name: &str) -> Parsed {
    let mut parsed = Parsed::default();
    let stem = strip_extension(name);

    // Brackets hold extras: a year, a version, a store name. None of it belongs to the title.
    let mut outside = String::new();
    let mut depth = 0usize;
    let mut inside = String::new();
    for c in stem.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    let extra = inside.trim();
                    match extra.parse::<i32>() {
                        Ok(year) if (1970..=2100).contains(&year) => parsed.year = Some(year),
                        _ if is_version(extra) => parsed.version = Some(extra.trim_start_matches(['v', 'V']).to_string()),
                        _ => {}
                    }
                    inside.clear();
                    outside.push(' ');
                }
            }
            _ if depth > 0 => inside.push(c),
            _ => outside.push(c),
        }
    }

    let mut words: Vec<String> = Vec::new();
    for token in outside.split(|c: char| c.is_whitespace() || c == '_').filter(|token| !token.is_empty()) {
        // "Stardew.Valley.v1.6.15": dots separate words, until what follows is a version.
        let parts: Vec<&str> = token.split('.').collect();
        let version_at = (0..parts.len()).find(|&i| parts.len() - i >= 2 && is_version(&parts[i..].join(".")));
        let (title_parts, version) = match version_at {
            Some(i) => (&parts[..i], Some(parts[i..].join("."))),
            None => (&parts[..], None),
        };
        if title_parts.len() > 1 && title_parts.iter().all(|part| part.chars().all(char::is_alphabetic) && part.len() > 1) {
            words.extend(title_parts.iter().map(|part| part.to_string()));
        } else if !title_parts.is_empty() {
            words.push(title_parts.join("."));
        }
        if let Some(version) = version {
            parsed.version = Some(version.trim_start_matches(['v', 'V']).to_string());
        }
    }

    let mut title: Vec<String> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let word = words[i].trim_matches(|c: char| c == '-' || c == ',');
        let lower = word.to_lowercase();
        if PATCH_WORDS.contains(&lower.as_str()) {
            parsed.patch = true;
            // "hotfix 31", "patch 7": the number after the word names the version.
            if let Some(number) = words.get(i + 1).filter(|next| next.chars().all(|c| c.is_ascii_digit() || c == '.')) {
                parsed.version.get_or_insert(format!("{lower} {number}"));
                i += 1;
            }
        } else if !word.is_empty() && !NOISE.contains(&lower.as_str()) {
            title.push(word.to_string());
        }
        i += 1;
    }
    parsed.title = title.join(" ");
    // A name typed all in lower case reads better with capitals: "tunic" becomes "Tunic".
    if !parsed.title.chars().any(char::is_uppercase) {
        parsed.title = parsed.title.split(' ').map(capitalize).collect::<Vec<_>>().join(" ");
    }
    if parsed.title.is_empty() {
        parsed.title = stem.to_string();
    }
    parsed
}

fn strip_extension(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, ext)) if crate::fsutil::GAME_FILE_EXTENSIONS.iter().any(|known| ext.eq_ignore_ascii_case(known)) => stem,
        _ => name,
    }
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// "1.0.2", "v2.0.16", "1.6.15a". A bare number like "2000" is not a version.
fn is_version(text: &str) -> bool {
    let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
    let digits_and_dots = text.trim_end_matches(|c: char| c.is_ascii_lowercase());
    text.len() - digits_and_dots.len() <= 1
        && digits_and_dots.contains('.')
        && digits_and_dots.split('.').all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

/// Lower case, letters and digits only, single spaces, no leading article.
/// "Baldur's Gate 3" and "baldurs gate 3" come out the same.
pub fn normalize(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if c != '\'' && c != '’' && !out.ends_with(' ') {
            out.push(' ');
        }
    }
    let out = out.trim();
    out.strip_prefix("the ").unwrap_or(out).to_string()
}

/// First letters of the words, digits kept: "Baldur's Gate 3" gives "bg3".
pub fn acronym(title: &str) -> String {
    normalize(title)
        .split(' ')
        .filter_map(|word| if word.chars().all(|c| c.is_ascii_digit()) { Some(word.to_string()) } else { word.chars().next().map(String::from) })
        .collect()
}

/// 0 to 1: how sure we are that a file named like `parsed` is the game `title` from `year`.
pub fn confidence(parsed: &Parsed, title: &str, year: Option<i32>) -> f64 {
    let (ours, theirs) = (normalize(&parsed.title), normalize(title));
    let mut score = if ours == theirs { 1.0 } else { strsim::normalized_levenshtein(&ours, &theirs) };
    match (parsed.year, year) {
        (Some(ours), Some(theirs)) if ours == theirs => score += 0.05,
        (Some(ours), Some(theirs)) if (ours - theirs).abs() > 1 => score -= 0.15,
        _ => {}
    }
    score.clamp(0.0, 1.0)
}

/// Whether a file named like `parsed` is a newer build of the game called `title`.
pub fn is_update_of(parsed: &Parsed, title: &str) -> bool {
    let (ours, theirs) = (normalize(&parsed.title), normalize(title));
    if ours.is_empty() {
        return false;
    }
    let same_game = ours == theirs || ours == acronym(title) && ours.len() >= 2;
    // Only a file that carries a version or says "patch" is an update. Anything else with
    // the same name is a second copy, and that is for an admin to sort out.
    same_game && (parsed.patch || parsed.version.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str) -> (String, Option<i32>, Option<String>, bool) {
        let parsed = parse(name);
        (parsed.title, parsed.year, parsed.version, parsed.patch)
    }

    #[test]
    fn reads_titles_years_and_versions_out_of_file_names() {
        assert_eq!(p("Tyrian 2000 (1999).zip"), ("Tyrian 2000".into(), Some(1999), None, false));
        assert_eq!(p("tunic_win_1.0.2.zip"), ("Tunic".into(), None, Some("1.0.2".into()), false));
        assert_eq!(p("Stardew.Valley.v1.6.15"), ("Stardew Valley".into(), None, Some("1.6.15".into()), false));
        assert_eq!(p("bg3_hotfix_31.zip"), ("Bg3".into(), None, Some("hotfix 31".into()), true));
        assert_eq!(p("Factorio 2.0.16"), ("Factorio".into(), None, Some("2.0.16".into()), false));
        assert_eq!(p("Half-Life 2 [GOG] (2004)"), ("Half-Life 2".into(), Some(2004), None, false));
        assert_eq!(p("setup_celeste_1.4.0.0.exe"), ("Celeste".into(), None, Some("1.4.0.0".into()), false));
        assert_eq!(p("Baldurs Gate 3"), ("Baldurs Gate 3".into(), None, None, false));
        assert_eq!(p("Celeste 64 Fragments of the Mountain.zip").0, "Celeste 64 Fragments of the Mountain");
        assert_eq!(p("Portal.2").0, "Portal.2", "a dot before a lone number is left alone");
    }

    #[test]
    fn scores_names_the_way_a_person_would() {
        let score = |file: &str, title: &str, year| confidence(&parse(file), title, year);
        assert_eq!(score("Tyrian 2000 (1999).zip", "Tyrian 2000", Some(1999)), 1.0);
        assert_eq!(score("tunic_win_1.0.2.zip", "TUNIC", None), 1.0);
        assert_eq!(score("Celeste 64 Fragments of the Mountain.zip", "Celeste 64: Fragments of the Mountain", None), 1.0);
        assert_eq!(score("Baldurs Gate 3", "Baldur's Gate 3", Some(2023)), 1.0);
        assert!(score("Tyrian 2000 (1999).zip", "Tyrian", Some(1995)) < 0.6);
        assert!(score("Celeste 64 Fragments of the Mountain.zip", "Celeste", None) < 0.6);
        assert!(score("Resident Evil 4 (2005)", "Resident Evil 4", Some(2023)) < 0.9, "the wrong year costs");
        assert!(score("Factorio", "Factory Town", None) < 0.85);
    }

    #[test]
    fn spots_updates_of_games_that_are_already_here() {
        assert!(is_update_of(&parse("bg3_hotfix_31.zip"), "Baldur's Gate 3"));
        assert!(is_update_of(&parse("Factorio 2.0.16"), "Factorio"));
        assert!(is_update_of(&parse("the_witness_patch_2.zip"), "The Witness"));
        assert!(!is_update_of(&parse("Factorio"), "Factorio"), "a second copy without a version is not an update");
        assert!(!is_update_of(&parse("Factorio 2.0.16"), "Satisfactory"));
        assert_eq!(acronym("Baldur's Gate 3"), "bg3");
        assert_eq!(normalize("The Witcher 3: Wild Hunt"), "witcher 3 wild hunt");
    }
}
