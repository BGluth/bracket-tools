use serde::Deserialize;
use thiserror::Error;

pub trait Provider {
    type ProviderQueryPayload;

    fn get<'de, Q, V>(&self, q: Q) -> V
    where
        Q: Into<Self::ProviderQueryPayload>,
        V: Deserialize<'de>;
}

#[derive(Clone, Debug, Error)]
pub enum ProviderError {}
