# BLNT Contract Specification Index

This index defines how the frozen Blend V2 behavioral baseline and the BLNT V3
contract specification compose. BLNT V3 is an immutable lending protocol
derived from Blend and designed to support real-world assets.

## Specification set

1. [V2_SYSTEM_SPEC.md](V2_SYSTEM_SPEC.md) defines the inherited behavior of
   the frozen SDK-22 Blend V2 lending, pool, and backstop contracts.
2. [V3_SYSTEM_SPEC.md](V3_SYSTEM_SPEC.md) defines only the V3 additions,
   replacements, extensions, approved exceptions, and safety fixes.

[V2_V3_PUBLIC_API.md](V2_V3_PUBLIC_API.md) is a non-normative review aid that
compares the public backstop, pool, and pool-factory entry points and data
types.

Declarative behavioral requirements and the terms `MUST`, `MUST NOT`,
`SHOULD`, and `MAY` in the normative specifications are acceptance criteria.

## Baseline and provenance

BLNT V3 is derived from Blend V2 source commit
`ba22b487b2c5057a4ecc28b05b5193c28e4bd117`. The frozen baseline uses Rust
1.81, Soroban SDK 22.0.7, and `wasm32-unknown-unknown`.

The V3 contracts use the toolchain selected by
[`rust-toolchain.toml`](../rust-toolchain.toml) and the exact dependencies in
[`Cargo.lock`](../Cargo.lock).

## Inheritance rule

BLNT V3 inherits every requirement and executable behavior in the frozen V2
baseline unless `V3_SYSTEM_SPEC.md` explicitly classifies a requirement as:

- **Added:** behavior with no V2 equivalent.
- **Replaced:** V3 behavior that supersedes a named V2 rule.
- **Extended:** a V2 rule generalized only as stated for V3.
- **Safety fix:** an intentional, approved deviation from V2.
- **Approved exception:** an intentional economic or administrative deviation
  approved in the V3 specification.

Omission from the V3 specification means inheritance, not unspecified
behavior. Restating an inherited rule for context does not create a V3
difference.

## Authority and conflict resolution

Apply the following order:

1. An explicit V3 addition or override, including a component requirement
   incorporated by `V3_SYSTEM_SPEC.md`, governs the V3 contracts.
2. Otherwise, the frozen V2 executable behavior governs.
3. `V2_SYSTEM_SPEC.md` documents that executable baseline.

If the written V2 specification conflicts with the frozen SDK-22 contracts or
tests, the executable baseline wins and the document MUST be corrected. A
conflict MUST NOT be resolved by silently creating a new V3 policy.
