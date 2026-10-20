//! Gagent core library.
//!
//! Gagent is a desktop coding agent for Gemini, NVIDIA-hosted models and any
//! OpenAI-compatible server. The agent works on a private
//! copy of your project (the *shadow workspace*), runs code on your own computer
//! (or in Docker / WSL when you prefer), and only touches your real folder when you
//! apply its changes.
//!
//! The library is UI-agnostic: the native desktop app (`gagent`) and the
//! headless command line tool (`gagent-cli`) are thin front ends over it.

pub mod agent;
pub mod config;
pub mod engine;
pub mod gemini;
pub mod gui;
pub mod host;
pub mod http;
pub mod llm;
pub mod modelstats;
pub mod models;
pub mod openai;
pub mod permission;
pub mod speech;
pub mod usage;
pub mod policy;
pub mod profiles;
pub mod prompt;
pub mod ratelimit;
pub mod router;
pub mod sandbox;
pub mod session;
pub mod shadow;
pub mod team;
pub mod tools;
pub mod util;
pub mod vision;
pub mod workspace;
pub mod wsl;

/// Application name shown in the UI and used for data directories.
pub const APP_NAME: &str = "Gagent";

/// Crate version, taken from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
