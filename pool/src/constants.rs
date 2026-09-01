/********** Numbers **********/

/// Fixed-point scalar for 12 decimal numbers
pub const SCALAR_12: i128 = 1_000_000_000_000;

/// Minimum bToken exchange rate that permits risk-increasing reserve actions
pub const MIN_OPERATIONAL_B_RATE: i128 = SCALAR_12 / 10;

/// Fixed-point scalar for 7 decimal numbers
pub const SCALAR_7: i128 = 1_0000000;

/// Immutable 0.2% fee applied to positive gross borrower-interest accrual.
pub const PROTOCOL_INTEREST_FEE_RATE: i128 = 20_000;

/// Seconds per year
pub const SECONDS_PER_YEAR: i128 = 31536000;

/// Seconds per week
pub const SECONDS_PER_WEEK: u64 = 604800;

/// Max amount of reserves that can be added to a pool
pub const MAX_RESERVES: u32 = 30;
