use crate::Result;
use crate::runtime::timeline::TimelineStore;

use crate::runtime::Runtime;

impl Runtime {
    pub fn new() -> Result<Self> {
        Self::from_store(TimelineStore::new(None)?)
    }

    pub fn from_store(timeline_store: TimelineStore) -> Result<Self> {
        Ok(Self { timeline_store })
    }

    pub async fn start(&mut self) -> Result<()> {
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<()> {
        Ok(())
    }
}
