use crate::provider::Provider;

pub struct GraphQlProvider {}

/// I don't know if I'll keep this type. Might just go with [String] in the end.
pub struct GraphQlKey;

impl Provider for GraphQlProvider {
    type Key = GraphQlKey;

    fn get<'de, V: serde::Deserialize<'de>>(&self, k: Self::Key) -> V {
        todo!()
    }
}
