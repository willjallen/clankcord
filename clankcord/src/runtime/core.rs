pub(crate) mod execution;
mod lifecycle;

use crate::runtime::timeline::TimelineStore;

#[derive(Debug, Clone)]
pub struct Runtime {
    pub timeline_store: TimelineStore,
}
