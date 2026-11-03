//! Shared helpers for memcore-api integration / E2E tests.
//!
//! Each integration test binary must declare `mod support;` to use this module.

#![allow(dead_code)]

pub mod assertions;
pub mod fixtures;
pub mod test_app;

pub use assertions::*;
pub use fixtures::*;
pub use test_app::TestApp;
