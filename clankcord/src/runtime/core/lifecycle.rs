use crate::Result;
use crate::runtime::Runtime;
use crate::runtime::timeline::TimelineStore;

impl Runtime {
    pub fn from_store(timeline_store: TimelineStore) -> Result<Self> {
        Ok(Self { timeline_store })
    }
}
