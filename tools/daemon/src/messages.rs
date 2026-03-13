use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum DaemonMsg {
    Shutdown,
    Purge(PurgeRequest),
    Recheck(RecheckRequest),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PurgeRequest {}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RecheckRequest {}
