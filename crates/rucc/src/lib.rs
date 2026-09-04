//! rucc, an optimizing C compiler.
//!
//! Design: `spec/00-README.md`. Layer rank 13, see `spec/18-package-layout.md`.
//!
//! This crate is the entry point, both for the `rucc` binary and for anyone embedding the
//! compiler. It re-exports the driver and nothing else: the pipeline crates are tier 3 in
//! `spec/18-package-layout.md` section 18.5 and are published so the workspace can be
//! published, not because their APIs are stable.
//!
//! The stable surface is the binary's command line behaviour, per section 18.5 tier 1.

#![doc(html_root_url = "https://docs.rs/rucc/0.3.12")]

pub use rucc_driver::{Action, CliError, USAGE, VERSION, parse_args, print_config, run};
