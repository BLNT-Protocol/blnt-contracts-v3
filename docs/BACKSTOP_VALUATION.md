# Blend V3 Backstop Valuation Specification

Status: Normative v3 component specification

This document composes with
[V3_SYSTEM_SPEC.md](V3_SYSTEM_SPEC.md) under the inheritance and precedence
rules in [SYSTEM_SPEC.md](SYSTEM_SPEC.md).

The backstop-valuation contract is the candidate's immutable pricing boundary
for BLND:USDC and BLND:XLM Comet LP tokens. It supplies canonical USDC values
for activation, status, take-rate allocation, bad-debt lots, interest lots,
and verified backstop exhaustion. It also exposes current underlying BLND;
Section 6.1 of the v3 specification independently defines the canonical spot
calculation used for reward-zone and emission weight.

## Immutable deployment configuration

The constructor binds the
[SEP-40 oracle consumer interface](https://github.com/stellar/stellar-protocol/blob/master/ecosystem/sep-0040.md)
as follows:

- one SEP-40 feed;
- a Stellar USDC feed base;
- BLND, USDC, and XLM token addresses;
- the exact BLND:USDC and BLND:XLM Comet addresses;
- a TWAP record count; and
- a maximum accepted price age.

The valuation contract verifies that all token addresses are distinct, all
five token interfaces use seven decimals, both Comets contain exactly the
expected pair at normalized weights 80% BLND and 20% paired asset, and the
feed advertises BLND and XLM. Its USDC base, precision, and resolution are
recorded and rechecked on every quote. It exposes the immutable BLND,
plain-USDC, BLND:USDC, and BLND:XLM binding. The candidate constructor rejects
the valuation contract unless that binding exactly matches its own immutable
asset arguments.

There is no administrator, price setter, privileged recovery path, alternate
oracle, or contract-upgrade method. A new pricing policy requires a separately
deployed contract version.

Construction and every public read extend the valuation contract's instance
and code TTL to 90 days whenever either falls below the 89-day threshold.
Version-only use therefore keeps the valuation contract live even when a pool
currently holds only plain USDC and no LP quote is required.

## Oracle policy

The valuation contract asks the immutable SEP-40 feed for the configured
number of BLND observations and, for BLND:XLM, the same number of XLM
observations.

- Record count is from 2 through 25.
- `resolution * (record_count - 1)` is from 30 minutes through 24 hours.
- Maximum age is at least one resolution and no more than one hour.
- Every price is positive.
- Every timestamp is nonfuture, resolution-aligned, and unique.
- The unordered observations must span the exact configured sequence of
  uniform ticks.
- The arithmetic mean rounds down.
- `valid_until` is the latest observation timestamp plus maximum age,
  inclusive. BLND:XLM returns the earlier BLND or XLM expiry.

Missing, malformed, changed, or stale oracle data fails the quote. Oracle
unavailability does not mean that backstop capital is exhausted and therefore
cannot authorize supplier loss.

## Conservative 80:20 Comet formula

All reserve and LP amounts use seven decimals. Oracle prices use the feed's
immutable precision \(D_o\). For BLND reserve \(R_b\), paired reserve \(R_q\),
LP supply \(S\), and prices \(P_b\) and \(P_q\):

\[
U_b = \left\lfloor \frac{R_bP_b}{10^{D_o}}\right\rfloor
\]

For BLND:USDC, \(U_q=R_q\). For BLND:XLM:

\[
U_q = \left\lfloor \frac{R_qP_q}{10^{D_o}}\right\rfloor
\]

Each side independently implies the total pool value at the fixed weights:

\[
T_b = \left\lfloor U_b\frac{10^7}{8{,}000{,}000}\right\rfloor
\]

\[
T_q = \left\lfloor U_q\frac{10^7}{2{,}000{,}000}\right\rfloor
\]

The USDC value of LP amount \(A\) is:

\[
V(A) =
\left\lfloor A\frac{\min(T_b,T_q)}{S}\right\rfloor
\]

The returned underlying BLND is:

\[
B(A) = \left\lfloor A\frac{R_b}{S}\right\rfloor
\]

At the 80:20 target both implied totals match. A one-sided reserve donation or
swap imbalance can only leave the value unchanged or reduce it, never increase
it. This avoids treating an instantaneous reserve ratio as an activation
price. A proportionate liquidity contribution increases reserves and supply
together and does not inflate value per LP share.

The `underlying_blnd` result intentionally follows current Comet composition.
That matches the separately approved spot-BLND emission policy. Price value
and emission weight are distinct outputs and MUST NOT be substituted for each
other. A positive LP amount may round to zero USDC value or zero underlying
BLND at seven-decimal precision. That dust contributes nothing to the
applicable policy but does not make every other pool position unpriceable.

Provider qualification, deployment simulation, and network evidence belong in
the separate
[Blend v3 migration repository](https://github.com/levinson/blend-v3-migration/blob/main/docs/ORACLE_DEPLOYMENT.md).
