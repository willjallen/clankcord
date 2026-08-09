use crate::Result;
use crate::model::job::{Job, JobState};
use crate::runtime::Ctx;

/// The children of a waiting parent, resolved into the three cases every
/// multi-child handler cares about. Consume-the-output specifics stay with
/// each handler; the scan and the failure message live here once.
#[allow(clippy::large_enum_variant)] // wire/decision enums: boxing buys nothing on the encoded form
pub(crate) enum ChildResolution {
    /// At least one child is still running: the parent keeps waiting.
    Pending,
    /// Every child is terminal and at least one did not complete.
    Failed {
        #[allow(dead_code)]
        child: Job,
        message: String,
    },
    /// Every child completed.
    Settled(Vec<Job>),
}

pub(crate) async fn await_children(ctx: &Ctx, job_id: &str, noun: &str) -> Result<ChildResolution> {
    let children = ctx.store.list_child_jobs(job_id).await?;
    Ok(resolve_children(children, noun))
}

pub(crate) fn resolve_children(children: Vec<Job>, noun: &str) -> ChildResolution {
    if children.iter().any(|child| !child.state.is_terminal()) {
        return ChildResolution::Pending;
    }
    if let Some(failed) = children
        .iter()
        .find(|child| child.state != JobState::Complete)
    {
        let message = format!(
            "{noun} {} ended as {}: {}",
            failed.id, failed.state, failed.metadata.error
        );
        return ChildResolution::Failed {
            child: failed.clone(),
            message,
        };
    }
    ChildResolution::Settled(children)
}
