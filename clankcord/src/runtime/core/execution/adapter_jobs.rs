use std::future::Future;
use std::pin::Pin;

use crate::Result;
use crate::runtime::timeline::TimelineStore;
use crate::runtime::{Job, JobOutput};

pub(crate) type AdapterJobFuture<'a> = Pin<Box<dyn Future<Output = Result<JobOutput>> + Send + 'a>>;

pub(crate) trait RuntimeAdapterJobs: Send + Sync {
    fn execute_adapter_job<'a>(&'a self, job: Job) -> AdapterJobFuture<'a>;

    fn execute_runtime_maintenance_job<'a>(
        &'a self,
        timeline_store: TimelineStore,
        job: Job,
    ) -> AdapterJobFuture<'a>;
}
