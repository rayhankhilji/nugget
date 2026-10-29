//! Support for the `knowledge` suite (M2): the labelled fixture corpus, the scripted fake LLM,
//! the adapter seam to the pipeline under test, and the scoring. The suite itself is
//! [`crate::suites::knowledge`]; methodology is in `bench/README.md`.

pub mod eval;
pub mod fake_llm;
pub mod fixture;
pub mod metered;
pub mod system;
pub mod wired;

#[cfg(test)]
pub(crate) mod oracle;
