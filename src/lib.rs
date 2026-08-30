pub mod cli;
pub mod hook;
pub mod integration;
pub mod model;
pub mod names;
pub mod procfs;
pub mod protocol;
pub mod server;
pub mod state;
pub mod tmux;

use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    fn now_unix_ms(&self) -> u64;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before Unix epoch")
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}
