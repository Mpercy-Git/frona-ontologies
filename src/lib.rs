//! Prepares published ontologies into small artifacts, and answers subsumption and
//! disjointness over them without materialising.
//!
//! Two halves, split by feature:
//!
//! - **The extractor** ([`graph`], [`rdf`]) — always available. An interned graph whose
//!   ancestor walk returns exactly what an OWL 2 RL reasoner derives for these artifacts,
//!   at ~0 allocation against a reasoner's gigabyte. `tests/equivalent_to_reasoner.rs`
//!   asserts that equality in both directions and is what makes it safe to depend on.
//! - **The pipeline** (everything else) — behind the default `cli` feature. Fetching,
//!   building, emitting, diffing, colouring.
//!
//! A consumer takes `default-features = false` and gets the first half with four
//! dependencies. The binary (`ont`) is a thin CLI over the second.
//!
//! It is a library at all because the failure mode here is *silence*: a broken step emits
//! a well-formed file with quietly missing content, so the guards in `tests/` must be able
//! to reach the graph rather than diff output files.

/// The extractor: interned graph, ancestor closure, disjointness. This is the half a
/// consumer wants, and the only half available with `default-features = false`.
pub mod graph;
pub mod rdf;

/// Building and inspecting artifacts — needs an HTTP client, a reasoner and a terminal.
#[cfg(feature = "cli")]
pub mod diff;
#[cfg(feature = "cli")]
pub mod emit;
#[cfg(feature = "cli")]
pub mod linkage;
#[cfg(feature = "cli")]
pub mod probe;
#[cfg(feature = "cli")]
pub mod project;
#[cfg(feature = "cli")]
pub mod recipe;
#[cfg(feature = "cli")]
pub mod term;
