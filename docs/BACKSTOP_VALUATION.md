# Blend V3 Backstop Valuation Specification

Status: Normative v3 component specification

This document composes with
[V3_SYSTEM_SPEC.md](V3_SYSTEM_SPEC.md) under the inheritance and precedence
rules in [SYSTEM_SPEC.md](SYSTEM_SPEC.md).

The backstop-valuation contract is the candidate's immutable pricing boundary
for BLND:USDC and BLND:XLM Comet LP tokens. It supplies Comet-implied USDC values
for activation, status, take-rate allocation, bad-debt lots, interest lots,
and verified backstop exhaustion. It also exposes current underlying BLND;
Section 6.1 of the v3 specification independently defines the canonical spot
calculation used for reward-zone and emission weight.

## Immutable deployment configuration

The constructor binds BLND, USDC, and XLM token addresses and the exact
BLND:USDC and BLND:XLM Comet addresses.

The valuation contract verifies that all token addresses are distinct, all
five token interfaces use seven decimals, both Comets contain exactly the
expected pair at normalized weights 80% BLND and 20% paired asset. It exposes
the immutable BLND, plain-USDC, BLND:USDC, and BLND:XLM binding. The candidate
constructor rejects the valuation contract unless that binding exactly matches
its own immutable asset arguments.

There is no oracle, administrator, price setter, privileged recovery path, or
contract-upgrade method. A new pricing policy requires a separately deployed
contract.

Construction and every public read extend the valuation contract's instance
and code TTL to 90 days whenever either falls below the 89-day threshold.
Any periodic public read can therefore keep the valuation contract live even
when a pool currently holds only plain USDC and no LP quote is required.

## Comet-implied 80:20 formula

All reserve and LP amounts use seven decimals. Let the BLND:USDC Comet hold
BLND reserve \(B_u\), USDC reserve \(U\), and LP supply \(S_u\). Its current
80:20 reserve composition implies total USDC value:

\[
T_u = 5U
\]

For BLND:USDC LP amount \(A_u\):

\[
V_u(A_u) = \left\lfloor A_u\frac{T_u}{S_u}\right\rfloor
\]

The same Comet implies a BLND price of \(4U/B_u\) USDC. Let the BLND:XLM
Comet hold BLND reserve \(B_x\) and LP supply \(S_x\). Its BLND side therefore
implies total USDC value:

\[
T_x = \left\lfloor\frac{5B_xU}{B_u}\right\rfloor
\]

For BLND:XLM LP amount \(A_x\):

\[
V_x(A_x) = \left\lfloor A_x\frac{T_x}{S_x}\right\rfloor
\]

The returned underlying BLND is:

\[
B(A) = \left\lfloor A\frac{R_b}{S}\right\rfloor
\]

Every quote rechecks positive LP supplies and reserves and the immutable 80:20
weights. Plain USDC is valued one-for-one by the backstop. Quotes do not expire;
`valid_until` is retained in the interface as `u64::MAX`.

These values deliberately reflect current Comet composition, not an external
fair-market price. Swaps, one-sided deposits or withdrawals, donations, and a
deauthorization that changes redemption behavior can change the implied value.
In particular, BLND:USDC remains the USDC anchor even if USDC is deauthorized.
The protocol does not automatically pause activation, emissions, take-rate
allocation, or auctions based on issuer authorization state. A zero or
otherwise invalid reserve fails closed because no ratio can be computed.

The `underlying_blnd` result also follows current Comet composition. Price
value and emission weight are distinct outputs and MUST NOT be substituted for
each other. A positive LP amount may round to zero USDC value or zero underlying
BLND at seven-decimal precision. That dust contributes nothing to the
applicable policy but does not make every other pool position unpriceable.
