#![recursion_limit = "512"]

pub mod adapters;
pub mod cli;
pub mod config;
pub mod dashboard;
pub mod errors;
pub mod runtime;

pub type Result<T> = anyhow::Result<T>;
