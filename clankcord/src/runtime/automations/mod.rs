mod engine;
mod manual;
mod room_agents;
mod spec;

pub(crate) use engine::{Automation, AutomationContext, AutomationOutput};
pub use engine::{AutomationJob, AutomationRun};
pub use manual::AUTOMATION_SPEC_MANUAL;
pub use spec::{
    AutomationAction, AutomationCondition, AutomationConditionOp, AutomationExpiry,
    AutomationOwner, AutomationRecord, AutomationResponseSink, AutomationResponseSinkKind,
    AutomationScalar, AutomationScope, AutomationSpec, AutomationState, AutomationTrigger,
};
