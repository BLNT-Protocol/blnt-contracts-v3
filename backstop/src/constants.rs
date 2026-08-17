/// Fixed-point scalar for 7 decimal numbers
pub const SCALAR_7: i128 = 1_0000000;

/// Fixed-point scalar for 14 decimal numbers
pub const SCALAR_14: i128 = 1_0000000_0000000;

/// The maximum reward zone size
pub const MAX_RZ_SIZE: u32 = 30;

/// The maximum amount of active Q4W entries that a user can have against a single backstop.
pub const MAX_Q4W_SIZE: u32 = 20;

/// The time in seconds that a Q4W entry is locked for (17 days).
pub const Q4W_LOCK_TIME: u64 = 17 * 24 * 60 * 60;

/// The maximum amount of backfilled emissions that can be emitted.
/// Represents between 3-4 months worth of token emissions.
pub const MAX_BACKFILLED_EMISSIONS: i128 = 10_000_000 * SCALAR_7;

/// The emitter's maximum one-time initial drop.
pub const MAX_INITIAL_DROP: i128 = 50_000_000 * SCALAR_7;

/// The verified USDC value required for pool activation.
pub const ACTIVATION_THRESHOLD_USDC: i128 = 12_500 * SCALAR_7;

/// Numerator and denominator for the one-percent USDC interest-proceeds haircut.
pub const USDC_BUYBACK_HAIRCUT_NUMERATOR: i128 = 1;
pub const USDC_BUYBACK_HAIRCUT_DENOMINATOR: i128 = 100;

/// Process at most 0.5% of the canonical Comet's current USDC reserve per buyback.
pub const BUYBACK_MAX_RESERVE_NUMERATOR: i128 = 1;
pub const BUYBACK_MAX_RESERVE_DENOMINATOR: i128 = 200;

/// Permit at most a one-percent increase over the Comet's current fee-inclusive spot price.
pub const BUYBACK_MAX_PRICE_NUMERATOR: i128 = 101;
pub const BUYBACK_MAX_PRICE_DENOMINATOR: i128 = 100;
