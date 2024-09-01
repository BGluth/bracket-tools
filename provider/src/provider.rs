use serde::Deserialize;
use thiserror::Error;

pub trait Provider {
    type Key;

    fn get<'de, V: Deserialize<'de>>(&self, k: Self::Key) -> V;
}

#[derive(Clone, Debug, Error)]
pub enum ProviderError {}
