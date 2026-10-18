//! Gagent core library.
//!
//! Gagent is a desktop coding agent for Gemini. The agent works on a private
//! copy of your project (the *shadow workspace*), runs code inside a Docker
//! sandbox, and only touches your real folder when you apply its changes.
//!
//! The library is UI-agnostic: the native desktop app (`gagent`) and the
//! headless command line tool (`gagent-cli`) are thin front ends over it.

pub mod agent;
pub mod config;
pub mod drive;
pub mod engine;
pub mod gemini;
pub mod gui;
pub mod http;
pub mod prompt;
pub mod sandbox;
pub mod session;
pub mod shadow;
pub mod tools;
pub mod util;
pub mod workspace;

/// Application name shown in the UI and used for data directories.
pub const APP_NAME: &str = "Gagent";

/// Crate version, taken from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
