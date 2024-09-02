use gg_query_core::types::GGRestToken;
use thiserror::Error;

use crate::types::Provider;

#[derive(Clone, Debug, Error)]
pub enum RestProviderError {}

pub type RestProviderResult<T> = Result<T, RestProviderError>;

#[derive(Debug)]
pub(crate) struct RestProvider {}

impl Provider for RestProvider {}

impl RestProvider {
    pub fn new(token: GGRestToken) -> RestProviderResult<Self> {
        todo!();
    }
}
