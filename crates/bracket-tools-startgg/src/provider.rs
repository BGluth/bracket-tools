use std::ops::Deref;

use bracket_tools_cache::{provider::Provider, query_cache::{CacheKey, CacheableKey}};
use serde::Deserialize;
use thiserror::Error;

use crate::{
    gg_data_types::{GgTournament, PlayerId, StartGgId, TournamentId},
    types::GGRestToken,
};

#[derive(Debug)]
pub struct GGQueryPayload {

}

#[derive(Debug, Error)]
pub enum GGProviderError {}

pub type GGProviderResult<T> = Result<T, GGProviderError>;

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
    type ProviderQueryPayload = GGQueryPayload;

    fn get<'de, Q, V>(&self, q: Q) -> V
    where
        Q: Into<Self::ProviderQueryPayload>,
        V: Deserialize<'de>,
    {
        todo!()
    }
}

impl<P: Provider> GGProvider<P> {
    pub fn get_player_id_for_name(p_name: &str) -> GGProviderResult<PlayerId> {
        todo!()
    }

    pub fn get_tournament_id_for_name(t_name: &str) -> GGProviderResult<TournamentId> {
        todo!()
    }

    pub fn get_tournaments_for_player(p_id: PlayerId) -> GGProviderResult<GgTournament> {
        todo!()
    }
}
