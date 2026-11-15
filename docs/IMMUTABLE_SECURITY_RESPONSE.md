# Immutable Deployment Security Response Plan

This plan applies to the immutable BLNT V3 mainnet contracts recorded in the
[core deployment registry](../deployments/mainnet-v3-core.json) and the
[Fixed-pool registry](../deployments/mainnet-v3-fixed-pool.json). It defines how
contributors should respond to a critical finding without assuming an
emergency administrator, proxy upgrade, privileged WASM replacement, or
undisclosed recovery authority.

## On-chain constraints

- The deployed pool, backstop, factory, emitter, and supporting contracts
  cannot be patched in place.
- Pool-local administrators retain only the authority exposed by the deployed
  pool ABI, including the bounded status and reserve-configuration operations.
  They are not protocol administrators and cannot replace contract code.
- Interfaces, keepers, indexers, and contributors can warn users or stop
  constructing affected transactions, but they cannot stop permissionless
  on-chain calls.
- The normal protocol-replacement path is the emitter's permissionless
  backstop migration: a candidate declares its immutable migration token,
  wins the strict balance comparison, remains queued for 31 days, and may be
  finalized during the following seven-day execution window.

## When to activate this plan

Treat a finding as critical when credible reproduction shows that it can cause
unauthorized asset movement or minting, material insolvency, corruption of
shared accounting, permanent loss of a risk-reducing exit, or compromise of
the emitter migration mechanism. A severe denial of service or manipulation
path may also be critical when realistic capital and timing assumptions make
material user loss likely.

Unconfirmed reports remain private to the smallest practical review group
until reproduction, impact, and disclosure risk are understood. This is a
coordination practice, not privileged control over the deployed contracts.

## Response sequence

1. **Preserve evidence.** Record the report, affected contract IDs, ledger and
   transaction references, observed state, source commit, deployed WASM hash,
   and the smallest known reproducer. Do not test an extractive path on
   mainnet.
2. **Reproduce and scope.** Reproduce against the exact deployed WASM in an
   isolated harness. Identify affected entry points, pools, assets, positions,
   and whether the emitter, backstop, or only a client integration is involved.
   Distinguish a code defect from stale oracle data, configuration, issuer
   behavior, or delayed permissionless maintenance.
3. **Take only existing bounded mitigations.** Publish a clear warning and
   remove or disable unsafe transaction construction in maintained interfaces.
   Where applicable, a pool's current administrator may place that pool in its
   deployed admin-frozen status after reviewing the effect on every user
   operation. Keepers should stop an affected action but continue independently
   verified risk-reducing maintenance. No response should imply that these
   off-chain actions pause the protocol globally.
4. **Choose the narrowest durable response.** If a bounded client-side
   workaround is safe, document and implement it across maintained SDK and UI
   paths. If deployed behavior must change, specify a V4 candidate containing
   the minimal correction, regression and invariant tests, exact release
   artifacts, and an independent review proportionate to the impact.
5. **Use the normal migration path when it remains trustworthy.** Publish the
   candidate source, specification, WASM hashes, constructor bindings, and
   deployment records. Verify its `migration_token()` declaration and model
   queue, cancellation, finalization, accounting, and user-position behavior.
   Economic support for the candidate and every queue or finalization call
   remain permissionless protocol actions; contributors cannot guarantee the
   outcome or bypass the 31-day queue and seven-day execution window.
6. **Do not assume migration if the emitter itself is affected.** If the
   finding makes the deployed emitter or its balance contest unsafe, the normal
   replacement path cannot be represented as a remedy. Any independent
   successor would require explicit voluntary user movement plus separate
   contracts, deployment, and security review. If it needs ongoing emissions,
   it may also need a new emissions asset because BLNT administration remains
   bound to the deployed emitter. It would not be an in-place upgrade of BLNT
   V3.
7. **Close with reproducible evidence.** Publish the impact analysis, affected
   ledgers, mitigations, remaining exposure, test results, review reports, and
   exact deployed artifacts. Keep sensitive exploit details private only as
   long as disclosure would create a material unresolved risk.

## Prepared evidence and maintenance

The committed deployment registries, exact source and dependency pins,
contract test suite, and the
[V2/V3 security comparison](V2_V3_SECURITY_COMPARISON.md) provide the starting
evidence for this process. Contributors should exercise the response sequence
before relying on it and update this plan whenever deployed migration semantics
or maintained response procedures change.
