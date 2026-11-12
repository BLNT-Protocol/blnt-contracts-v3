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

### Canonical mainnet addresses

The canonical BLNT asset is
[`BLNT:GAP2ACGH3O6TRFAERGM4USJ6XWU3U2BFIK224JPV6NI3CUNEEEGTBTFB`](https://stellar.expert/explorer/public/asset/BLNT-GAP2ACGH3O6TRFAERGM4USJ6XWU3U2BFIK224JPV6NI3CUNEEEGTBTFB).

| Component | Contract address | Deployment record |
| --- | --- | --- |
| BLNT asset (SAC) | [`CCBNWP5L7ZSXA3DBWCMFAVKFVRW3ZHWO7B5J557W42O64DDZNPJP7VDS`](https://stellar.expert/explorer/public/contract/CCBNWP5L7ZSXA3DBWCMFAVKFVRW3ZHWO7B5J557W42O64DDZNPJP7VDS) | [BLNT asset](deployments/mainnet-v3-token.json) |
| Legacy BLND asset (SAC) | [`CD25MNVTZDL4Y3XBCPCJXGXATV5WUHHOWMYFF4YBEGU5FCPGMYTVG5JY`](https://stellar.expert/explorer/public/contract/CD25MNVTZDL4Y3XBCPCJXGXATV5WUHHOWMYFF4YBEGU5FCPGMYTVG5JY) | [Core contracts](deployments/mainnet-v3-core.json) |
| USDC asset (SAC) | [`CCW67TSZV3SSS2HXMBQ5JFGCKJNXKZM7UQUWUZPUTHXSTZLEO7SJMI75`](https://stellar.expert/explorer/public/contract/CCW67TSZV3SSS2HXMBQ5JFGCKJNXKZM7UQUWUZPUTHXSTZLEO7SJMI75) | [Core contracts](deployments/mainnet-v3-core.json) |
| XLM asset (SAC) | [`CAS3J7GYLGXMF6TDJBBYYSE3HQ6BBSMLNUQ34T6TZMYMW2EVH34XOWMA`](https://stellar.expert/explorer/public/contract/CAS3J7GYLGXMF6TDJBBYYSE3HQ6BBSMLNUQ34T6TZMYMW2EVH34XOWMA) | [Core contracts](deployments/mainnet-v3-core.json) |
| Emitter | [`CDSO6FQSBJ3U3AJHBNNLHHKSMKTTDDN3GLGF3KZTMXBFV7SVB43ZJERT`](https://stellar.expert/explorer/public/contract/CDSO6FQSBJ3U3AJHBNNLHHKSMKTTDDN3GLGF3KZTMXBFV7SVB43ZJERT) | [Core contracts](deployments/mainnet-v3-core.json) |
| BLNT backfill | [`CANIE64BDFKLHROLOIY2KTQVBWUZNUKZP75DLXC272QM7TIN5N3M4LVA`](https://stellar.expert/explorer/public/contract/CANIE64BDFKLHROLOIY2KTQVBWUZNUKZP75DLXC272QM7TIN5N3M4LVA) | [Core contracts](deployments/mainnet-v3-core.json) |
| Pool factory | [`CAKLFWAHB2RIYRBSPUL6EH43AIQ5ILBEM7HTMWYSJRL7PHZQ676KMCV5`](https://stellar.expert/explorer/public/contract/CAKLFWAHB2RIYRBSPUL6EH43AIQ5ILBEM7HTMWYSJRL7PHZQ676KMCV5) | [Core contracts](deployments/mainnet-v3-core.json) |
| Backstop | [`CAHCUKBPTW5DR5RNXOHCTJD5U76A6EEZXYZAQFRQEEQRXQ3CRUZ6Z3MO`](https://stellar.expert/explorer/public/contract/CAHCUKBPTW5DR5RNXOHCTJD5U76A6EEZXYZAQFRQEEQRXQ3CRUZ6Z3MO) | [Core contracts](deployments/mainnet-v3-core.json) |
| BLNT:USDC Comet V1.1 LP | [`CDQMIKIHKA5UY3FQJ2UDTLNX3GOPD3GPNZWSPTOBVOHEUMQJQXOPC7Y3`](https://stellar.expert/explorer/public/contract/CDQMIKIHKA5UY3FQJ2UDTLNX3GOPD3GPNZWSPTOBVOHEUMQJQXOPC7Y3) | [Canonical Comet V1.1 pools](deployments/mainnet-v3-comet.json) |
| BLNT:XLM Comet V1.1 LP | [`CA5HX3D47PQZWC2E7VWXCKH4534P2NOJEOIXAWU47MI4QV2FQUYY3FLC`](https://stellar.expert/explorer/public/contract/CA5HX3D47PQZWC2E7VWXCKH4534P2NOJEOIXAWU47MI4QV2FQUYY3FLC) | [Canonical Comet V1.1 pools](deployments/mainnet-v3-comet.json) |
| Fixed Pool V3 | [`CACSQWCMKG45TUHCQ3LXHOB7YEEJH4W376ZHQPXH2IZ3TE4NJHFZMCY7`](https://stellar.expert/explorer/public/contract/CACSQWCMKG45TUHCQ3LXHOB7YEEJH4W376ZHQPXH2IZ3TE4NJHFZMCY7) | [Fixed Pool V3](deployments/mainnet-v3-fixed-pool.json) |
| Fixed Pool oracle | [`CCVTVW2CVA7JLH4ROQGP3CU4T3EXVCK66AZGSM4MUQPXAI4QHCZPOATS`](https://stellar.expert/explorer/public/contract/CCVTVW2CVA7JLH4ROQGP3CU4T3EXVCK66AZGSM4MUQPXAI4QHCZPOATS) | [Fixed Pool V3](deployments/mainnet-v3-fixed-pool.json) |
| YieldBlent Pool | [`CAUTPSQDAJEUV5XEU3EXFVYRGDJLEJGVTFJCFZCSPTDCREAMZ573VF4D`](https://stellar.expert/explorer/public/contract/CAUTPSQDAJEUV5XEU3EXFVYRGDJLEJGVTFJCFZCSPTDCREAMZ573VF4D) | [YieldBlent Pool](deployments/mainnet-v3-yieldblent-pool.json) |
| YieldBlent oracle | [`CDN7JD6SP32RZHB3DHK4S4AWCCQF6GYIUO455UCR4RGRZILZDRD24N36`](https://stellar.expert/explorer/public/contract/CDN7JD6SP32RZHB3DHK4S4AWCCQF6GYIUO455UCR4RGRZILZDRD24N36) | [YieldBlent Pool](deployments/mainnet-v3-yieldblent-pool.json) |

Pool-specific reserve assets are recorded in their respective pool deployment
records and are not canonical protocol contracts.

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
