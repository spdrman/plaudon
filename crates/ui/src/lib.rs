//! egui widgets for the Plaudon desktop shell.
//!
//! The crate is deliberately thin: a widget here renders state and emits
//! intent, and every rule it obeys — what may carry a timestamp, what one undo
//! step is, when a citation goes stale, where a pixel is in time — lives in a
//! core crate that knows nothing about egui. That is what keeps the rules
//! testable headlessly and the UI replaceable.
//!
//! Modules are added by the EPIC O tasks as they land; this file stays a plain
//! list of them so the tasks do not collide in it.

#![deny(missing_docs)]

pub mod activity;
pub mod ai;
pub mod jobs;
pub mod onboarding;
pub mod player;
pub mod screens;
pub mod settings;
pub mod shell;
pub mod theme;
pub mod transcript;
pub mod widgets;
