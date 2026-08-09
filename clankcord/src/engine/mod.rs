//! The generic job engine: submission, scheduling, dispatch, and the
//! recurring-work clock. Knows the JobSpec table and the payload routes,
//! never individual business logic.

mod bus;
mod decision;
pub mod dispatcher;
pub(crate) mod routes;
pub(crate) mod scheduler;
pub mod schedules;

pub use bus::JobBus;
pub use decision::JobDecision;
pub(crate) use scheduler::RuntimeExecutor;
