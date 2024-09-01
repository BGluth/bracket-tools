use std::time::Duration;

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
    rate_limiter: DefaultDirectRateLimiter,
}

/// I don't know if I'll keep this type. Might just go with [String] in the end.
pub struct GraphQlKey;

impl Provider for GraphQlProvider {
    type Key = GraphQlKey;

    fn get<'de, V: serde::Deserialize<'de>>(&self, k: Self::Key) -> V {
        todo!()
    }
}

#[derive(Debug)]
pub struct Rate {
    amt: usize,
    duration: Duration,
}
