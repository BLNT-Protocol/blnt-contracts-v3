# Contract Baseline

## Provenance

Blend v3 is derived from the Blend v2 source at commit
`ba22b487b2c5057a4ecc28b05b5193c28e4bd117`.

The frozen v2 baseline uses:

- Rust 1.81;
- Soroban SDK 22.0.7; and
- `wasm32-unknown-unknown`.

The v3 contracts use the toolchain pinned by
[`rust-toolchain.toml`](../rust-toolchain.toml) and the exact dependencies in
[`Cargo.lock`](../Cargo.lock). V2 behavior is documented in
[V2_SYSTEM_SPEC.md](V2_SYSTEM_SPEC.md); intentional v3 differences are
documented in [V3_SYSTEM_SPEC.md](V3_SYSTEM_SPEC.md).
