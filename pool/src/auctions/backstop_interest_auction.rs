use crate::pool::{Pool, User};
use soroban_sdk::{Address, Env, Vec};

use super::AuctionData;

/// Create the next unique-backstop-token interest auction.
pub fn create_interest_auction_data(e: &Env, lot_assets: &Vec<Address>) -> AuctionData {
    super::tier_interest::create_interest_auction_data(e, lot_assets)
}

/// Return the active interest auction's v2-compatible public projection.
pub fn get_interest_auction(e: &Env) -> AuctionData {
    super::tier_interest::get_interest_auction(e)
}

/// Delete an interest auction after staleness or immediate deauthorization.
pub fn delete_interest_auction(e: &Env) {
    super::tier_interest::delete_interest_auction(e)
}

/// Fill the active interest auction using the inherited auction curve.
pub fn fill_interest_auction(
    e: &Env,
    pool: &mut Pool,
    auction_user: &Address,
    filler_state: &User,
    percent: u32,
) -> AuctionData {
    super::tier_interest::fill_interest_auction(e, pool, auction_user, filler_state, percent)
}
