# BLNT V3

This repository contains the source for BLNT V3, an immutable lending protocol
derived from Blend and designed to support real-world assets. BLNT V3 is
independently developed and is not an official Blend release.

## Documentation

The contract specification is composed by
[docs/SYSTEM_SPEC.md](docs/SYSTEM_SPEC.md). It inherits the frozen behavior in
[docs/V2_SYSTEM_SPEC.md](docs/V2_SYSTEM_SPEC.md) and defines intentional V3
changes in [docs/V3_SYSTEM_SPEC.md](docs/V3_SYSTEM_SPEC.md).

## Audit status

The V3-specific contract changes have not yet undergone an independent
security audit.

## Immutability

The deployed BLNT V3 contracts are immutable. The production source and
dependency pins in this repository are frozen to the exact deployed WASMs.
Contract behavior changes require a separately identified future protocol
version and migration path; BLNT V3 cannot be patched in place.

## Mainnet deployment evidence

Canonical mainnet addresses, creation transactions and timestamps, artifact
hashes, constructor bindings, and verification evidence are maintained in this
repository. The primary records cover:

- [BLNT asset](deployments/mainnet-v3-token.json)
- [Canonical Comet V1.1 pools](deployments/mainnet-v3-comet.json)
- [Core contracts](deployments/mainnet-v3-core.json)
- [Fixed Pool V3](deployments/mainnet-v3-fixed-pool.json)
- [YieldBlent Pool](deployments/mainnet-v3-yieldblent-pool.json)

## Reserve-token trust

Pools may support generic SEP-41 reserve assets. A generic or upgradeable token
contract is a trusted dependency of its pool. Exact balance checks reject
observable transfer discrepancies, but cannot detect a token that reports false
balances or later changes its behavior. Users should review every reserve's
implementation, upgrade authority, administrative and issuer controls, mutable
dependencies, and oracle before using a pool. Generic SEP-41 reserves are
identified in the BLNT interface but are not automatically quarantined.

## Build and test

The checked-in `rust-toolchain.toml` selects Rust 1.91.1 and the
`wasm32v1-none` target automatically for commands run in this repository.

Build the optimized contract WASMs:

```bash
make
```

Artifacts are written to `target/wasm32v1-none/optimized`. The build includes
the pool factory, V3 BLNT emitter, backstop, and pool. The emitter preserves the
legacy backstop-replacement API and emits BLNT directly to V3 without replacing
the V1/V2 BLND emitter. Comet V1.1-based backstop valuation is integrated into
the backstop. The committed `comet.wasm` is the official
`v1.1.0_contracts_pkg1.0.0_cli25.1.0` release artifact with SHA-256
`d735c3395f59510172cf5cf838823a1389b97cfec6bf24e580e9bd77d2b3e687`.
The build rejects any production
WASM larger than 120,000 bytes, preserving the Protocol 27 deployment headroom
established by BLNT V3 deployment testing.

Compare the optimized artifacts with the exact deployed V2 contracts:

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
