#![recursion_limit = "512"]

pub mod adapters;
pub mod cli;
pub mod config;
pub mod dashboard;
pub mod engine;
pub mod errors;
pub mod ports;
pub mod runtime;
pub mod views;

pub type Result<T> = anyhow::Result<T>;
