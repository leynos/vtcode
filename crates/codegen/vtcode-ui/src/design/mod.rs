//! Centralized design system for VT Code.
//!
//! This crate provides a single source of truth for all design system concerns:
//! - **Colour conversion**: Unified `anstyle` to `ratatui` colour mapping
//! - **Style bridging**: Conversion between styling frameworks
//! - **Design constants**: Shared UI constants (ellipses, spacing, breakpoints)
//! - **Layout**: Responsive layout mode logic
//! - **Panel**: Base panel widget primitive
//! - **Diff formatting**: Unified diff rendering with ANSI colours

pub(crate) mod colour;
pub mod constants;
pub mod diff;
pub mod layout;
pub mod panel;
pub(crate) mod style;
