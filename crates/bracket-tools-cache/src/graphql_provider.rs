use std::{sync::Arc, time::Duration};

use governor::DefaultDirectRateLimiter;
use thiserror::Error;

use crate::provider::Provider;

/// The default maximum request rate for the start.gg API.
const GG_DEFAULT_REQ_RATE: Rate = Rate {
    amt: 80,
    duration: Duration::from_mins(1),
};

#[derive(Debug, Error)]
pub enum GraphQlProviderError {}

#[derive(Debug)]
pub struct GraphQlScheduler {}

#[derive(Debug)]
pub struct GraphQlQuery {
    endpoint: String,

    /// Header to also include (optional).
    header: Option<Vec<(String, String)>>,
}

#[derive(Debug)]
pub struct GraphQlProviderBuilder {
    max_rate: Rate,
}

impl Default for GraphQlProviderBuilder {
    fn default() -> Self {
        Self {
            max_rate: GG_DEFAULT_REQ_RATE,
        }
    }
}

impl GraphQlProviderBuilder {
    pub fn max_req_rate(mut self, max_rate: Rate) -> Self {
        self.max_rate = max_rate;
        self
    }

    pub fn build(self) -> GraphQlProvider {
        todo!()
    }
}

#[derive(Debug)]
pub struct GraphQlProvider {
    rate_limiter: Arc<Box<DefaultDirectRateLimiter>>,
}

impl Provider for GraphQlProvider {
    type ProviderQueryPayload = GraphQlQuery;

    fn get<'de, Q, V>(&self, q: Q) -> V
    where
        Q: Into<Self::ProviderQueryPayload>,
        V: serde::Deserialize<'de>,
    {
        todo!()
    }
}

#[derive(Debug)]
pub struct Rate {
    amt: usize,
    duration: Duration,
}
