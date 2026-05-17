mod engine;
mod manual;
mod room_agents;
mod spec;

pub(crate) use engine::{Automation, AutomationContext, AutomationOutput, AutomationVoiceState};
pub use engine::{AutomationJob, AutomationRun};
pub use manual::AUTOMATION_SPEC_MANUAL;
pub use spec::{
    AutomationAction, AutomationCondition, AutomationConditionOp, AutomationDelay,
    AutomationExpiry, AutomationOwner, AutomationPendingRecheck, AutomationRecord,
    AutomationScalar, AutomationScope, AutomationSpec, AutomationState, AutomationTextTarget,
    AutomationTextTargetKind, AutomationTrigger,
};
