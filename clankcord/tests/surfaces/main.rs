//! Presented-output contracts: CLI copy and usage, dashboard payloads and
//! health reasons, HTTP routes, slash responses, rendered transcripts.

#[path = "../support/mod.rs"]
mod support;

mod cli;
mod dashboard_frontend;
mod dashboard_health;
mod dashboard_http;
mod dashboard_queries;
mod slash;
mod transcripts;
