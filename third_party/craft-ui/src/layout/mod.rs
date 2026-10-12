// Adapted from storytold/craft-libs, crates/ui/src/layout/mod.rs @ 1aa81a773aa9198a0e4c0daeda954d622475af38
// Copyright (c) 2026 ArtCraft Team and the craft-libs contributors
// Licensed under MIT OR Apache-2.0. Modified: the toolbar-fitting module is not vendored.

//! Bounded pane geometry and resize handles, independent of workspace serialization.
//!
//! Applications retain their dock trees, fixed-versus-relative sizing, commands and pane contents.
//! [`split_rect`] lays out one split, and [`ResizeHandle`] reports an interaction without mutating
//! that application state.

mod geometry;
mod resize;

pub use geometry::{split_rect, SplitAxis, SplitLayout, SplitSize};
pub use resize::{ResizeHandle, ResizeHandleStyle, ResizeResponse};

#[cfg(test)]
mod tests;
