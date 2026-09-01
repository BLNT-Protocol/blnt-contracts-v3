use crate::pool::{Pool, User};
use soroban_sdk::{Address, Env};

use super::{AuctionData, TierAuctionData};

/// Create the next canonical bad-debt auction.
pub fn create_bad_debt_auction_data(e: &Env) -> TierAuctionData {
    super::tier_bad_debt::create_bad_debt_auction_data(e)
}

/// Return the active bad-debt auction and its privately selected tier.
pub fn get_bad_debt_auction(e: &Env) -> TierAuctionData {
    super::tier_bad_debt::get_bad_debt_auction(e)
}

/// Fill the active bad-debt auction using the inherited auction curve.
pub fn fill_bad_debt_auction(
    e: &Env,
    pool: &mut Pool,
    auction_user: &Address,
    filler_state: &mut User,
    percent: u32,
) -> AuctionData {
    super::tier_bad_debt::fill_bad_debt_auction(e, pool, auction_user, filler_state, percent)
}

/// Default residual backstop debt only after every tier is exhausted.
pub fn default_backstop_bad_debt(e: &Env) {
    super::tier_bad_debt::default_backstop_bad_debt(e);
}
