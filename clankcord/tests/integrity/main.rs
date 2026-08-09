//! Durable-contract guarantees: wire-format round-trips, claim atomicity
//! and ordering, lineage, retention, exactly-once schedules, restart survival.

#[path = "../support/mod.rs"]
mod support;
mod timeline_durability;
mod wake_circuit;
