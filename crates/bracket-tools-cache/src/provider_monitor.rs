//! Consider finding a better name for this...

use governor::DefaultDirectRateLimiter;

pub trait ProviderRequest {
    fn sent_data_stats() -> SentDataStats;
}

#[derive(Debug)]
pub struct SentDataStats {
    sent_bytes: u64,
}

pub trait ProviderResponse {
    fn received_data_stats() -> ReceivedDataStats;
}

#[derive(Debug)]
pub struct ReceivedDataStats {
    received_bytes: u64,
}

#[derive(Debug)]
pub(crate) struct ProviderMonitor {
    rate_limiter: DefaultDirectRateLimiter,
    stats: ProviderStatCollector,
}

impl ProviderMonitor {
    pub(crate) fn queue_req(&self) {
        todo!()
    }
}

#[derive(Debug)]
struct ProviderStatCollector {}
