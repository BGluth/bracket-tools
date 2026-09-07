//! Sleep, timeout and wall-clock reads that work under tokio and in the
//! browser, so the poll and write loops run unchanged in both shells.

use cfg_if::cfg_if;
use web_time::{SystemTime, UNIX_EPOCH};

use crate::conflict::UnixMillis;

pub fn now_millis() -> UnixMillis {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

cfg_if! {
    if #[cfg(target_arch = "wasm32")] {
        use std::{future::Future, pin::pin, time::Duration};

        use futures::future::{select, Either};

        /// The future did not complete within its timeout.
        #[derive(Debug)]
        pub struct Elapsed;

        pub async fn sleep(duration: Duration) {
            gloo_timers::future::sleep(duration).await;
        }

        pub async fn timeout<F: Future>(duration: Duration, future: F) -> Result<F::Output, Elapsed> {
            match select(pin!(future), pin!(sleep(duration))).await {
                Either::Left((output, _)) => Ok(output),
                Either::Right(_) => Err(Elapsed),
            }
        }
    } else {
        pub use tokio::time::{error::Elapsed, sleep, timeout};
    }
}
