//! Tournament slug forms. start.gg accepts `tournament/foo` in queries; users
//! paste bare slugs and full URLs; series siblings share a slug stem.

/// Accepts a bare slug, `tournament/foo`, or a full start.gg URL; returns the
/// pinned `tournament/foo` form.
pub fn normalize_tournament_slug(input: &str) -> String {
    let trimmed = input.trim().trim_end_matches('/');
    if let Some(ix) = trimmed.find("tournament/") {
        let rest = &trimmed[ix + "tournament/".len()..];
        let slug = rest.split('/').next().unwrap_or(rest);
        return format!("tournament/{slug}");
    }
    format!("tournament/{trimmed}")
}

/// The bare slug (`weekly-100`) from any accepted tournament form.
pub fn bare_slug(input: &str) -> String {
    normalize_tournament_slug(input).trim_start_matches("tournament/").to_string()
}

/// The series stem of a bare slug: `weekly-100` → `weekly`. A slug without a
/// trailing number is its own stem.
pub fn series_stem(bare: &str) -> &str {
    match bare.rfind('-') {
        Some(ix) if !bare[ix + 1..].is_empty() && bare[ix + 1..].chars().all(|c| c.is_ascii_digit()) => &bare[..ix],
        _ => bare,
    }
}

#[cfg(test)]
mod tests {
    use super::{bare_slug, normalize_tournament_slug, series_stem};

    #[test]
    fn series_stems() {
        assert_eq!(series_stem("french-bread-rumble-100"), "french-bread-rumble");
        assert_eq!(series_stem("fbr-9"), "fbr");
        assert_eq!(series_stem("weekly"), "weekly");
        assert_eq!(series_stem("smash-64-arena"), "smash-64-arena");
        assert_eq!(series_stem("trailing-dash-"), "trailing-dash-");
    }

    #[test]
    fn bare_slugs() {
        assert_eq!(bare_slug("https://www.start.gg/tournament/fbr-100/details"), "fbr-100");
        assert_eq!(bare_slug("tournament/fbr-100"), "fbr-100");
        assert_eq!(bare_slug("fbr-100"), "fbr-100");
    }

    #[test]
    fn normalized_slugs() {
        for input in [
            "fbr-100",
            "tournament/fbr-100",
            "tournament/fbr-100/",
            "https://www.start.gg/tournament/fbr-100/details",
            "https://www.start.gg/tournament/fbr-100/",
        ] {
            assert_eq!(normalize_tournament_slug(input), "tournament/fbr-100");
        }
    }
}
