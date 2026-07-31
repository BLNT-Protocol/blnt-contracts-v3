# Blend Contract Specification Index

This index defines how the frozen Blend v2 behavioral baseline and the
experimental Blend v3 contract specification compose.

## Specification set

1. [V2_SYSTEM_SPEC.md](V2_SYSTEM_SPEC.md) defines the inherited behavior of
   the frozen SDK-22 Blend v2 lending, pool, and backstop contracts.
2. [V3_SYSTEM_SPEC.md](V3_SYSTEM_SPEC.md) defines only the v3 additions,
   replacements, extensions, approved exceptions, and safety fixes.

Declarative behavioral requirements and the terms `MUST`, `MUST NOT`,
`SHOULD`, and `MAY` in the normative specifications are acceptance criteria.

## Baseline and provenance

Blend v3 is derived from Blend v2 source commit
`ba22b487b2c5057a4ecc28b05b5193c28e4bd117`. The frozen baseline uses Rust
1.81, Soroban SDK 22.0.7, and `wasm32-unknown-unknown`.

The v3 contracts use the toolchain selected by
[`rust-toolchain.toml`](../rust-toolchain.toml) and the exact dependencies in
[`Cargo.lock`](../Cargo.lock).

## Inheritance rule

Blend v3 inherits every requirement and executable behavior in the frozen v2
baseline unless `V3_SYSTEM_SPEC.md` explicitly classifies a requirement as:

- **Added:** behavior with no v2 equivalent.
- **Replaced:** v3 behavior that supersedes a named v2 rule.
- **Extended:** a v2 rule generalized only as stated for v3.
- **Safety fix:** an intentional, approved deviation from v2.
- **Approved exception:** an intentional economic or administrative deviation
  approved in the v3 specification.

Omission from the v3 specification means inheritance, not unspecified
behavior. Restating an inherited rule for context does not create a v3
difference.

## Authority and conflict resolution

Apply the following order:

1. An explicit v3 addition or override, including a component requirement
   incorporated by `V3_SYSTEM_SPEC.md`, governs the v3 contracts.
2. Otherwise, the frozen v2 executable behavior governs.
3. `V2_SYSTEM_SPEC.md` documents that executable baseline.

If the written v2 specification conflicts with the frozen SDK-22 contracts or
tests, the executable baseline wins and the document MUST be corrected. A
conflict MUST NOT be resolved by silently creating a new v3 policy.
