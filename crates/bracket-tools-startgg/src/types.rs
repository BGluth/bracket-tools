use std::str::FromStr;

use thiserror::Error;

const NUM_BYTES_IN_TOKEN: usize = 20;

#[derive(Clone, Debug)]
pub struct GGRestToken([u8; NUM_BYTES_IN_TOKEN]);

#[derive(Debug, Error)]
pub enum GGRestTokenParseError {
    #[error("The string {0} can not be parsed as hex! (Underlying reason: {1})")]
    NotHex(String, String),

    #[error("The GG token should be exactly 20 bytes (40 characters) but instead found {1} {2}! (token: {0})")]
    UnexpectedLength(String, usize, &'static str),
}

impl FromStr for GGRestToken {
    type Err = GGRestTokenParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Remove `0x` prefix if present.
        let lower = s.to_lowercase();
        let cleaned_str = lower.trim_start_matches("0x");

        let bytes = hex::decode(cleaned_str).map_err(|err| GGRestTokenParseError::NotHex(s.to_string(), err.to_string()))?;

        if bytes.len() != NUM_BYTES_IN_TOKEN {
            let bytes_or_byte_str = Self::get_bytes_or_byte_str(bytes.len());

            return Err(GGRestTokenParseError::UnexpectedLength(
                s.to_string(),
                bytes.len(),
                bytes_or_byte_str,
            ));
        }

        let mut token_buf = [0; NUM_BYTES_IN_TOKEN];
        token_buf.copy_from_slice(&bytes);

        Ok(GGRestToken(token_buf))
    }
}

impl GGRestToken {
    /// Returns the token as a `"Bearer <hex>"` string for use in HTTP Authorization headers.
    pub fn as_bearer_value(&self) -> String {
        format!("Bearer {}", hex::encode(self.0))
    }

    fn get_bytes_or_byte_str(n_bytes: usize) -> &'static str {
        match n_bytes {
            1 => "byte",
            _ => "bytes",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{GGRestToken, GGRestTokenParseError};

    const PASSING_STRS: [&str; 2] = [
        "0x11223344556677889900aabbccddeeffbbeeeeff",
        "0X11223344556677889900aabbccddEEFFbbeeeeFF",
    ];

    const FAILING_STRS: [&str; 3] = ["", "0x11223344AA", "0xCheese"];

    #[test]
    fn gg_token_parsing() {
        for s in PASSING_STRS {
            parse_to_token(s).expect("Unable to parse token string!");
        }

        for s in FAILING_STRS {
            assert!(
                parse_to_token(s).is_err(),
                "Expected the string \"{}\" to be unable to be parsed to a token!",
                s
            );
        }
    }

    fn parse_to_token(s: &str) -> Result<GGRestToken, GGRestTokenParseError> {
        GGRestToken::from_str(s)
    }

    #[test]
    fn bearer_value_round_trips() {
        let token = GGRestToken::from_str("0x11223344556677889900aabbccddeeffbbeeeeff").unwrap();
        assert_eq!(
            token.as_bearer_value(),
            "Bearer 11223344556677889900aabbccddeeffbbeeeeff"
        );
    }
}
