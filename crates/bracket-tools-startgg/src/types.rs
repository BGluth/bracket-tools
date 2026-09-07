use std::{fs, io, path::Path, str::FromStr};

use thiserror::Error;

#[derive(Clone, Debug)]
pub struct GGRestToken(String);

#[derive(Debug, Error)]
pub enum GGRestTokenFileError {
    #[error("reading token file {path}: {source}")]
    Read { path: String, source: io::Error },
    #[error("invalid token in {path}: {source}")]
    Parse { path: String, source: GGRestTokenParseError },
}

#[derive(Debug, Error)]
pub enum GGRestTokenParseError {
    #[error("Token is empty")]
    Empty,

    #[error("Invalid character {0:?} at position {1} (only visible ASCII 0x21..=0x7E allowed)")]
    InvalidCharacter(char, usize),
}

impl FromStr for GGRestToken {
    type Err = GGRestTokenParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();

        if trimmed.is_empty() {
            return Err(GGRestTokenParseError::Empty);
        }

        for (i, c) in trimmed.char_indices() {
            if !(('\x21'..='\x7E').contains(&c)) {
                return Err(GGRestTokenParseError::InvalidCharacter(c, i));
            }
        }

        Ok(GGRestToken(trimmed.to_string()))
    }
}

impl GGRestToken {
    /// Reads a token from a file (surrounding whitespace ignored).
    pub fn from_file(path: &Path) -> Result<Self, GGRestTokenFileError> {
        let display = path.display().to_string();
        let raw = fs::read_to_string(path).map_err(|source| GGRestTokenFileError::Read {
            path: display.clone(),
            source,
        })?;

        Self::from_str(&raw).map_err(|source| GGRestTokenFileError::Parse { path: display, source })
    }

    /// Returns the token as a `"Bearer <token>"` string for use in HTTP Authorization headers.
    pub fn as_bearer_value(&self) -> String {
        format!("Bearer {}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{GGRestToken, GGRestTokenParseError};

    #[test]
    fn parses_real_format_token() {
        let token = GGRestToken::from_str("91b0c4b4aeae0a040d5b2c0e4d8861c2").unwrap();
        assert_eq!(token.0, "91b0c4b4aeae0a040d5b2c0e4d8861c2");
    }

    #[test]
    fn parses_short_token() {
        GGRestToken::from_str("abc123").unwrap();
    }

    #[test]
    fn parses_non_hex_token() {
        GGRestToken::from_str("some-opaque-token-value!").unwrap();
    }

    #[test]
    fn rejects_empty() {
        let err = GGRestToken::from_str("").unwrap_err();
        assert!(matches!(err, GGRestTokenParseError::Empty));
    }

    #[test]
    fn rejects_whitespace_only() {
        let err = GGRestToken::from_str("   ").unwrap_err();
        assert!(matches!(err, GGRestTokenParseError::Empty));
    }

    #[test]
    fn rejects_control_characters() {
        let err = GGRestToken::from_str("token\ninjection").unwrap_err();
        assert!(matches!(err, GGRestTokenParseError::InvalidCharacter('\n', _)));
    }

    #[test]
    fn rejects_spaces_in_token() {
        let err = GGRestToken::from_str("token with spaces").unwrap_err();
        assert!(matches!(err, GGRestTokenParseError::InvalidCharacter(' ', _)));
    }

    #[test]
    fn bearer_round_trip() {
        let token = GGRestToken::from_str("91b0c4b4aeae0a040d5b2c0e4d8861c2").unwrap();
        assert_eq!(token.as_bearer_value(), "Bearer 91b0c4b4aeae0a040d5b2c0e4d8861c2");
    }

    #[test]
    fn trims_surrounding_whitespace() {
        let token = GGRestToken::from_str("  abc123  ").unwrap();
        assert_eq!(token.0, "abc123");
    }
}
