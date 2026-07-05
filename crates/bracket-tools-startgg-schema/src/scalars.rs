use crate::schema::schema;

/// The start.gg `Timestamp` scalar: unix seconds, returned by the API as a JSON
/// number.
///
/// Older query modules carry their own `Timestamp(pub String)`; multiple Rust
/// types may map to the same GraphQL scalar, so those remain untouched. New
/// queries should use this shared numeric representation.
#[derive(cynic::Scalar, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamp(pub i64);

#[cfg(test)]
mod tests {
    use super::Timestamp;

    #[test]
    fn deserializes_from_json_number() {
        let ts: Timestamp = serde_json::from_str("1751234567").unwrap();
        assert_eq!(ts, Timestamp(1751234567));
    }

    #[test]
    fn serializes_to_json_number() {
        assert_eq!(serde_json::to_string(&Timestamp(1751234567)).unwrap(), "1751234567");
    }
}
