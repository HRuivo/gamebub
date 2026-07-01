use std::sync::{LazyLock, Mutex, MutexGuard};

static CORE_MANAGER: LazyLock<Mutex<CoreManager>> =
    LazyLock::new(|| Mutex::new(CoreManager::new()));

pub struct CoreManager {}

impl CoreManager {
    fn new() -> Self {
        CoreManager {}
    }

    pub fn lock() -> MutexGuard<'static, Self> {
        CORE_MANAGER.lock().unwrap()
    }

    /// Start the process of running a specific core (by ID).
    pub fn run_core(&mut self, id: &str) {
        log::info!("Run core: {id}");
        // TODO
    }
}
