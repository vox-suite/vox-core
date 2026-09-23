/**
* Health check endpoints and liveness monitoring for worker instances.
*/
use std::sync::atomic::{AtomicBool, Ordering};

#[allow(dead_code)]
pub struct WorkerHealth {
    is_healthy: AtomicBool,
}

#[allow(dead_code)]
impl WorkerHealth {
    pub fn new() -> Self {
        Self {
            is_healthy: AtomicBool::new(true),
        }
    }

    pub fn set_healthy(&self, val: bool) {
        self.is_healthy.store(val, Ordering::SeqCst);
    }

    pub fn is_healthy(&self) -> bool {
        self.is_healthy.load(Ordering::SeqCst)
    }
}
