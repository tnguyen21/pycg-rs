//! Integration tests for pycg-rs.
//!
//! Uses Python test fixtures in tests/test_code/.

#[path = "integration/common.rs"]
mod common;
#[path = "integration/core.rs"]
mod core;
#[path = "integration/features.rs"]
mod features;
#[path = "integration/fixture_coverage.rs"]
mod fixture_coverage;
#[path = "integration/library_surface.rs"]
mod library_surface;
#[path = "integration/corpus.rs"]
mod corpus;
#[path = "integration/accuracy.rs"]
mod accuracy;
