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

/// The verified USDC value required for an inactive pool to activate.
pub const ACTIVATION_ENTRY_THRESHOLD_USDC: i128 = 12_500 * SCALAR_7;

/// The verified USDC value an active pool must maintain.
pub const ACTIVATION_MAINTENANCE_THRESHOLD_USDC: i128 = 10_000 * SCALAR_7;
