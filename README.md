# Blend Protocol V3 Candidate

This repository contains the experimental Blend Protocol v3 candidate
contracts. It is derived from Blend v2 and is not an official or
production-ready Blend release.

## Documentation

The contract specification is composed by
[docs/SYSTEM_SPEC.md](docs/SYSTEM_SPEC.md). It inherits the frozen behavior in
[docs/V2_SYSTEM_SPEC.md](docs/V2_SYSTEM_SPEC.md) and defines intentional v3
changes in [docs/V3_SYSTEM_SPEC.md](docs/V3_SYSTEM_SPEC.md).

Migration compatibility, deployment runners, and network evidence live in the
separate
[blend-v3-candidate repository](https://github.com/levinson/blend-v3-candidate).

## Audits

Conducted audits can be viewed in the `audits` folder.

## Getting Started

The checked-in `rust-toolchain.toml` selects Rust 1.91.1 and the
`wasm32v1-none` target automatically for commands run in this repository.

Build the contracts with:

```
make
```

Run all unit tests and the integration test suite with:

```
make test
```

## Deployment

The `make` command creates an optimized and un-optimized set of WASM contracts. It's recommended to use the optimized version if deploying to a network.

These can be found at the path:

```
target/wasm32v1-none/optimized
```

The build includes the pool factory, backstop, pool, and immutable v3
backstop valuation contract. It also rejects an optimized backstop larger than
120,000 bytes, preserving the Protocol 27 deployment headroom established by
the candidate's deployment testing.

For help with deployment to a network, please visit the [Blend Utils](https://github.com/blend-capital/blend-utils) repo.

## Contributing

Notes for contributors:

- Under no circumstances should the "overflow-checks" flag be removed otherwise contract math will become unsafe

## Community Links

A set of links for various things in the community. Please submit a pull request if you would like a link included.

- [Blend Discord](https://discord.com/invite/a6CDBQQcjW)
