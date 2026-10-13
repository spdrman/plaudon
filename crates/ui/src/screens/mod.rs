//! One module per destination of the shell.
//!
//! Each screen is thin: it renders the DTOs `plaudon-app` returns and emits
//! intents, and the host turns an intent into a command. Modules are added by
//! the EPIC O tasks as they land; this file stays a plain list of them so the
//! tasks do not collide in it.

pub mod devices;
pub mod inbox;
pub mod library;
pub mod people;
pub mod recording;
pub mod search;
pub mod sources;
pub mod templates;
