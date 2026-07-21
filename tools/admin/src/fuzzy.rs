//! Tiny dependency-free fuzzy matcher for player tags.

/// Match quality, weakest to strongest (declaration order drives `Ord`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchTier {
    Subsequence,
    Substring,
    Prefix,
    Exact,
}

impl MatchTier {
    pub fn label(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Prefix => "prefix",
            Self::Substring => "substr",
            Self::Subsequence => "fuzzy",
        }
    }
}

/// Case-insensitive tier of `query` against one target string.
pub fn match_tier(query: &str, target: &str) -> Option<MatchTier> {
    let query = query.to_lowercase();
    let target = target.to_lowercase();
    if query.is_empty() || target.is_empty() {
        return None;
    }

    if target == query {
        Some(MatchTier::Exact)
    } else if target.starts_with(&query) {
        Some(MatchTier::Prefix)
    } else if target.contains(&query) {
        Some(MatchTier::Substring)
    } else if is_subsequence(&query, &target) {
        Some(MatchTier::Subsequence)
    } else {
        None
    }
}

/// The best tier of `query` across several surfaces (tag, prefix, combined).
pub fn best_tier<'a>(query: &str, surfaces: impl IntoIterator<Item = &'a str>) -> Option<MatchTier> {
    surfaces.into_iter().filter_map(|surface| match_tier(query, surface)).max()
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut pending = needle.chars().peekable();
    for c in haystack.chars() {
        if pending.peek() == Some(&c) {
            pending.next();
        }
    }
    pending.peek().is_none()
}

#[cfg(test)]
mod tests {
    use super::{best_tier, match_tier, MatchTier};

    #[test]
    fn tiers_rank_by_specificity() {
        assert_eq!(match_tier("mang0", "Mang0"), Some(MatchTier::Exact));
        assert_eq!(match_tier("mang", "Mang0"), Some(MatchTier::Prefix));
        assert_eq!(match_tier("ang", "Mang0"), Some(MatchTier::Substring));
        assert_eq!(match_tier("mg0", "Mang0"), Some(MatchTier::Subsequence));
        assert_eq!(match_tier("zelda", "Mang0"), None);
        assert!(MatchTier::Exact > MatchTier::Prefix);
        assert!(MatchTier::Prefix > MatchTier::Substring);
        assert!(MatchTier::Substring > MatchTier::Subsequence);
    }

    #[test]
    fn empty_sides_never_match() {
        assert_eq!(match_tier("", "Mang0"), None);
        assert_eq!(match_tier("mang0", ""), None);
    }

    #[test]
    fn best_tier_takes_the_strongest_surface() {
        let tier = best_tier("c9", ["Mang0", "C9", "C9 Mang0"]);
        assert_eq!(tier, Some(MatchTier::Exact));

        assert_eq!(best_tier("nobody", ["Mang0", "C9"]), None);
    }
}
