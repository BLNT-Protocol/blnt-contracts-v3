# Claim retry against captured mainnet state

This test runs the backstop WASM built from the current checkout against a saved
BLNT mainnet system. Kanatoko 29.1.2 loads the state, locally substitutes that WASM
at the existing backstop address, and runs the same scenario against both the
deployed and source-built code. Their settlement results must match.

The production workspace stays on SDK 27.0.3 and Rust 1.91.1. This separate test
workspace uses a Protocol 29 host because the saved ledger is Protocol 29. It
does not link the production Rust crates or change their dependency resolution.
The generated client and replacement code both come from
`target/wasm32v1-none/optimized/backstop.wasm`, built by `make` before testing.

## Scenario and results

The existing YieldBlent position belongs to
`GDJSH2NU2WF6J4P5DL4522DUCABWSTZOKFQ7BHBCFYQ3QKC6FRYWP6OL` in pool
`CAUTPSQDAJEUV5XEU3EXFVYRGDJLEJGVTFJCFZCSPTDCREAMZ573VF4D`.
Its FirstLoss tier compounds into the canonical BLNT:XLM Comet pool.

A local trader sells 1% of the Comet BLNT reserve. That actual Comet swap changes
the LP output of the pending claim enough to cross a 0.5% minimum. The stale
minimum rejects with contract error 20 and leaves the entire ledger unchanged.
A retry with a refreshed positive minimum receives exactly the previewed LP and
credits the same user's originating tier. A further claim receives zero.

The following is an example run from ledger 65,561,470. A new capture can have
different amounts; the test checks the same invariants and deployed/source
agreement instead of requiring these historical numbers.

| Measurement | Deployed and source-built backstop |
| --- | ---: |
| Accrued reward consumed once | 0.5999136 BLNT |
| Claim output before trade | 0.0520627 LP |
| Old minimum | 0.0518023 LP |
| Claim output after trade | 0.0515472 LP |
| Refreshed minimum | 0.0512894 LP |
| Successful retry | 0.0515472 LP |
| Shares credited | 0.0515472 shares |
| Trader input | 53,527.3300953 BLNT |
| Trader output | 2,241.6062021 XLM |

The test checks exact reward/LP custody deltas, share conversion using the saved
pool's share/token ratio, unchanged queued withdrawals and other-tier position,
and equality of both final positions. Storage and TTLs are unchanged when only
the backstop code is substituted. This tests expected behavior, not a new defect.

## Fixture and authorization

Preparation captures state directly from the public Stellar RPC using Kanatoko
and saves it in the user's local cache, outside this repository. It requires no
GitHub account, contributor-hosted artifact or shared fixture download. Repeating
preparation keeps an existing snapshot; it never silently refreshes it.

Replay validates the bundle's integrity digests, mainnet identity, Protocol 29,
and the deployed backstop, emitter and both Comet WASM hashes. Those code hashes
come from the [canonical deployment registry](../../deployments/mainnet-v3-core.json)
and official Comet artifact. The deployed backstop hash is
`0469cfb445bedd87478274f265c8776b32cde1e8840fca4684ffa1dc4d5a750f`.
The test prints the ledger number, ledger hash and file SHA-256 with
`--nocapture`. Keeping the same file reproduces the same state. A fresh capture
requires this position to have positive claimable rewards and a supported ledger;
unavailable state or changed deployments fail rather than substituting mock data.

Protocol balances, positions, rewards and reserves come from the snapshot.
Only the trader is synthetic: Kanatoko explicitly funds it with 10 XLM, creates
a BLNT trustline, and mints the trade input in a local token-admin context.
Owner/trader authorization is mocked using local source-account contexts.
No real private key, signature, public-network transaction or upgrade is involved.
Local code replacement does not demonstrate an authorized upgrade path.

## Run

With the repository toolchain and Stellar CLI available:

```sh
make prepare-mainnet-fixture
make test-mainnet
```

The first preparation uses read-only RPC calls. The replay is an explicit local
test target and runs offline from that snapshot. The existing `make test` and CI
workflow retain their original behavior and do not require a capture. Missing,
corrupt or incompatible bundles fail. Dependencies may need downloading on the
first Cargo invocation.

To select an existing capture explicitly:

```sh
make test-mainnet MAINNET_FIXTURE=/absolute/path/to/core-claim-v1.json
```

The ignored capture test is an explicit, read-only maintenance operation. After
building the contracts, it can write a new bundle and compare both code versions:

```sh
BLNT_CAPTURE_OUTPUT=/absolute/path/to/a-new-file.json \
  cargo test --manifest-path test-suites/mainnet/Cargo.toml --locked \
  capture_fixture -- --ignored --nocapture
```

Use a different output path when intentionally capturing another ledger. The
test reports each capture's identity and keeps the deployed-code checks fixed.
