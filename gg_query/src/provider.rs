use provider::provider::Provider;
use serde::Deserialize;

use crate::types::GGRestToken;

#[derive(Clone, Debug)]
pub struct GGProvider<P: Provider> {
    provider: P,
    token: GGRestToken,
}

impl<P: Provider> GGProvider<P> {
    pub fn new(gg_token: GGRestToken, p: P) -> Self {
        todo!()
    }
}

impl<P: Provider> Provider for GGProvider<P> {
    type Key = u64;

    fn get<'de, V: Deserialize<'de>>(&self, k: Self::Key) -> V {
        todo!()
    }
}
