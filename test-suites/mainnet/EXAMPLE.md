# Repay during an oracle outage

This test protects partial repayment and collateral top-ups when YieldBlent's
configured oracle stops providing an XLM price. Further borrowing and collateral
withdrawals must fail atomically, while the borrower can still reduce risk.

Kanatoko 29.1.2 executes the actual pool, aggregator, Reflector and asset contracts
from a mainnet capture. It repeats the scenario with the optimized pool WASM
built from the current checkout at the same address. Both runs must satisfy the
assertions and finish with identical storage and TTLs, excluding the pool's code.

## Additional coverage

The common oracle mock's `set_price_stable` returns the current ledger timestamp,
so time jumps do not make those prices unavailable. The existing
[`test_submit_no_liabilities_does_not_load_oracle`](../../pool/src/pool/submit.rs#L1121)
finishes with no debt. Here **debt remains positive during the outage**.
The aggregator's own 900/901-second boundary is already tested upstream; this
scenario covers BLNT's continued risk-reducing operations across that boundary.

## Scenario and results

The local borrower supplies 100 XLM and borrows 1 USDC from the deployed
YieldBlent pool. No pool reserves, oracle prices or existing positions are
overwritten. Time is advanced relative to the captured XLM price timestamp,
without introducing new Reflector rounds.

| Operation | Result for deployed and source-built pool |
| --- | --- |
| Borrow another 0.01 USDC at price age 900 seconds | Succeeds; wallet has 1.01 USDC |
| Read XLM price at age 901 seconds | Unavailable; USDC base price remains 1 |
| Borrow 0.01 USDC or withdraw 1 XLM collateral | Aborts; entire ledger unchanged |
| Repay 0.5 USDC with debt remaining | Exact wallet debit, pool credit and debt-share reduction |
| Add 10 XLM collateral with debt remaining | Exact custody/share increase; debt shares unchanged |

Example from ledger **65,562,065**, with XLM round timestamp `1795096200`:

```text
candidate=false oracle_round=1795096200 boundary=1795097100 debt_shares=10090772->5095343 partial_repay=5000000 added_collateral=100000000
candidate=true oracle_round=1795096200 boundary=1795097100 debt_shares=10090772->5095343 partial_repay=5000000 added_collateral=100000000
```

Amounts above use seven decimal places. New captures can produce different
debt shares; the test derives expected changes from the captured reserve rates.
The unavailable-price path aborts while the pool unwraps `None`, before its
separate one-day stale-price check. This is expected behavior, not a new defect.

### Regression sensitivity

A temporary mutation moved `PositionData::calculate_from_positions` before the
`check_health` condition, leaving health assertions conditional. This plausible
eager-valuation refactor makes partial repayment depend on the collateral oracle.
At base `771059f6dd232fd35ea29b7b2a987f0e13bb2372`:

- All 586 existing workspace tests passed with the mutated Rust source.
- The 46 integration tests were then repeated with a freshly built mutated
  optimized pool WASM and passed. They are a subset of the 586, not extra tests.
- This replay passed for the deployed control and failed on the candidate's
  0.5-USDC repayment because the aggregator returned no XLM price.
- Restoring the original pool made the replay pass again.

The mutation is not included. The deployed contract already behaves correctly.

## Run

With the repository toolchain and Stellar CLI available:

```sh
make prepare-mainnet-fixture
make test-mainnet
```

Preparation reads public Stellar RPC into a local cache outside the repository.
It reuses an existing file without refreshing it; no GitHub account or hosted
snapshot is needed. Replay is offline. Dependencies may need downloading on the
first Cargo invocation. The existing `make test` and CI workflow are unchanged;
they do not automatically run this opt-in test.

To use another saved capture or intentionally prepare a new one, pass
`MAINNET_FIXTURE=/absolute/path/to/oracle-outage.json` to the same commands.
Choose a new path when refreshing. Preparation requires a supported ledger,
a fresh XLM price and sufficient liquidity/configuration for the small loan;
it fails instead of inventing missing state.

## Scope and reproducibility

The test validates bundle integrity, mainnet/Protocol 29 identity, the deployed
pool and aggregator hashes from the [registry](../../deployments/mainnet-v3-yieldblent-pool.json),
and captured WASM bytes against instance references, including Reflector.
Reflector's hash is not pinned because that dependency is upgradeable. Keeping
the same capture preserves its exact code and state. The test prints the ledger
identity, capture SHA-256, source WASM hash and outcomes.
Missing, corrupt or incompatible snapshots fail.

Only the deterministic borrower, its initial 1,000 XLM, USDC trustline and local
authorization are synthetic. No real keys, signatures or network transactions
are used. Moving local time models absent feed updates, not a prediction about
future mainnet state. Code substitution is local, not a deployed upgrade path.

The production SDK 27 workspace and build settings remain unchanged. The
separate test package uses a Protocol 29 host to match the captured ledger.
Local source-built WASM hashes differ from the deployment registry; this test
establishes behavior for this scenario, not complete artifact reproducibility
or general equivalence of deployed and source-built code.
