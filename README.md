# Blend Protocol V3 Candidate

This repository contains the experimental Blend Protocol v3 candidate
contracts. It is derived from Blend v2 and is not an official or
production-ready Blend release.

## Documentation

The contract specification is composed by
[docs/SYSTEM_SPEC.md](docs/SYSTEM_SPEC.md). It inherits the frozen behavior in
[docs/V2_SYSTEM_SPEC.md](docs/V2_SYSTEM_SPEC.md) and defines intentional v3
changes in [docs/V3_SYSTEM_SPEC.md](docs/V3_SYSTEM_SPEC.md).

## Audit status

The v3-specific contract changes have not yet undergone an independent
security audit.

## Build and test

The checked-in `rust-toolchain.toml` selects Rust 1.91.1 and the
`wasm32v1-none` target automatically for commands run in this repository.

Build the optimized contract WASMs:

```bash
make
```

Artifacts are written to `target/wasm32v1-none/optimized`. The build includes
the pool factory, backstop, and pool; Comet-based backstop valuation is
integrated into the backstop. It rejects any production WASM larger than
120,000 bytes, preserving the Protocol 27 deployment headroom established by
the candidate's deployment testing.

Compare the optimized artifacts with the exact deployed v2 contracts:

```bash
make wasm-sizes
```

Continuous integration publishes the Markdown report for every commit. The
immutable comparison inputs and their artifact hashes are recorded in
`wasm-size-v2-baseline.json`.

Run the complete native and integration test suite:

```bash
make test
```
