/// Fixed-point scalar for 7 decimal numbers
pub const SCALAR_7: i128 = 1_0000000;

/// Maximum one-time drop for the immutable launch backstop.
pub const MAX_INITIAL_DROP: i128 = 150_000_000 * SCALAR_7;

/// Maximum combined discretionary and backfill drop for later backstops.
pub const MAX_MIGRATION_DROP: i128 = 50_000_000 * SCALAR_7;
