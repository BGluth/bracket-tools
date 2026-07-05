use std::fmt;

use serde::{de::Visitor, Deserialize, Deserializer, Serialize, Serializer};

use crate::schema::schema;

/// The start.gg `Timestamp` scalar: unix seconds, returned by the API as a JSON
/// number.
///
/// Older query modules carry their own `Timestamp(pub String)`; multiple Rust
/// types may map to the same GraphQL scalar, so those remain untouched. New
/// queries should use this shared numeric representation.
#[derive(cynic::Scalar, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamp(pub i64);

/// The start.gg `ID` scalar. The SDL types it as `ID` (a string per the
/// GraphQL spec) but the live API returns most ids as JSON **numbers**, with
/// string forms for preview ids (e.g. `preview_…`). `cynic::Id` rejects the
/// numeric form, so new query modules use this tolerant type instead:
/// deserializes from either shape, canonicalized to a string.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(String);

// Manual expansion of `cynic::impl_scalar!`: the TypeLock for the built-in
// `ID` scalar is `cynic::Id` itself, which the macro can't name (it assumes
// the lock lives in the schema module).
impl cynic::schema::IsScalar<cynic::Id> for Id {
    type SchemaType = cynic::Id;
}

impl cynic::coercions::CoercesTo<cynic::Id> for Id {}

impl schema::variable::Variable for Id {
    const TYPE: cynic::variables::VariableType = cynic::variables::VariableType::Named(<cynic::Id as cynic::schema::NamedType>::NAME);
}

impl Id {
    pub fn new(inner: impl Into<String>) -> Self {
        Self(inner.into())
    }

    pub fn inner(&self) -> &str {
        &self.0
    }
}

impl Serialize for Id {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct IdVisitor;

        impl Visitor<'_> for IdVisitor {
            type Value = Id;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string or integer id")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Id, E> {
                Ok(Id(v.to_owned()))
            }

            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Id, E> {
                Ok(Id(v.to_string()))
            }

            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Id, E> {
                Ok(Id(v.to_string()))
            }
        }

        deserializer.deserialize_any(IdVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::{Id, Timestamp};

    #[test]
    fn deserializes_from_json_number() {
        let ts: Timestamp = serde_json::from_str("1751234567").unwrap();
        assert_eq!(ts, Timestamp(1751234567));
    }

    #[test]
    fn serializes_to_json_number() {
        assert_eq!(serde_json::to_string(&Timestamp(1751234567)).unwrap(), "1751234567");
    }

    #[test]
    fn id_deserializes_from_json_number() {
        let id: Id = serde_json::from_str("104940737").unwrap();
        assert_eq!(id, Id::new("104940737"));
    }

    #[test]
    fn id_deserializes_from_json_string() {
        let id: Id = serde_json::from_str("\"preview_123_4\"").unwrap();
        assert_eq!(id.inner(), "preview_123_4");
    }

    #[test]
    fn id_serializes_as_string() {
        assert_eq!(serde_json::to_string(&Id::new("42")).unwrap(), "\"42\"");
    }
}
