# Changelog

All notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [semantic versioning](https://semver.org/spec/v2.0.0.html) with the caveat in `spec/18-package-layout.md` section 18.6: pre-1.0 versions carry no compatibility promise at all.

## Unreleased

### Added

- The workspace: 23 library crates, the `rucc` binary, the rule DSL and its verifier under `build-tools/`, and the target-side runtime under `runtime/`.
- The layer rule, ranked in `xtask/layers.toml` and enforced by `cargo xtask layers`.
- `rucc --print-config`, `--version` and `--help`, which is the M0 exit criterion in `spec/17-milestones.md`.
- Target triple parsing and the target data model for x86-64, AArch64 and RISC-V 64 across Linux, Apple platforms and Windows.
- Diagnostics, spans and the per-compilation `Session`.
- CI on Linux, macOS and Windows, with formatting, lints, tests, the layer check, the prose check, a supply chain audit and a minimum supported Rust version job.
- The twenty document specification under `spec/`.

Nothing compiles C yet. The frontend lands in M1 and M2.
