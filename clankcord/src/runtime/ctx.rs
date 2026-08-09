use crate::engine::JobBus;
use crate::runtime::timeline::TimelineStore;

/// Capability context handed to job handlers and read surfaces.
///
/// Handlers are free functions taking `&Ctx`; cross-module calls are visible
/// imports instead of methods on a shared god object.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub store: TimelineStore,
    pub bus: JobBus,
}

impl Ctx {
    pub fn new(store: TimelineStore) -> Self {
        let bus = JobBus::new(store.clone());
        Self { store, bus }
    }
}
