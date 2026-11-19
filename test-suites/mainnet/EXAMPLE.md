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

The [external fixture](https://github.com/roman-karpovich/blend-utils/blob/7b407135d50202796bf9206b47cec86650a6a853/pool/claim-replay/fixtures/core-claim-v1.json)
is pinned to a full commit and SHA-256
`58946b483f90329c020a25680cb4bc5217f8ace4a93493d8a2df74680d524e11`.
It contains 27 present and 3 confirmed-absent entries from mainnet ledger
65,561,470, hash
`e8074b39029b335ebfa104b069bd5beb9f5df46daf7e8a1bdb5f07b5ee8d0427`.
The deployed backstop code hash is
`0469cfb445bedd87478274f265c8776b32cde1e8840fca4684ffa1dc4d5a750f`.
The bundle stays outside this repository and is downloaded into the user's cache.
The fixture's BLNT/Comet code retains its AGPL provenance in the external package.

Protocol balances, positions, rewards and reserves come from the snapshot.
Only the trader is synthetic: Kanatoko explicitly funds it with 10 XLM, creates
a BLNT trustline, and mints the trade input in a local token-admin context.
Owner/trader authorization is mocked using local source-account contexts.
No real private key, signature, public-network transaction or upgrade is involved.
Local code replacement does not demonstrate an authorized upgrade path.

## Run

With the repository toolchain, Stellar CLI and authenticated GitHub CLI available:

```sh
make prepare-mainnet-fixture
make test-mainnet
```

`make test` includes this replay as well as the existing workspace tests and fuzz
check. The existing PR workflow prepares the fixture separately, then runs that
command. Tests never call RPC or refresh the fixture; a missing or altered bundle
fails. Dependencies may need downloading on the first Cargo invocation.

To select an already downloaded bundle explicitly:

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

Capture does not update the pin or published artifact. Any refreshed fixture
requires review of its ledger, code hashes, result table and checksum.
