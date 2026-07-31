use sep_41_token::TokenClient;
use soroban_sdk::{contracttype, panic_with_error, Address, BytesN, Env, I256};

use crate::{constants::SCALAR_7, emissions, storage, BackstopError};

use super::{
    quote_lp_amount, require_registered_pool, tier_token, update_tier_totals, AssetValuation,
    BackstopTier,
};

const ONE_DAY_LEDGERS: u32 = 17_280;
const AUCTION_TTL_THRESHOLD: u32 = 45 * ONE_DAY_LEDGERS;
const AUCTION_TTL_BUMP: u32 = 46 * ONE_DAY_LEDGERS;
const BAD_DEBT_LOT_PREMIUM_NUMERATOR: i128 = 6;
const BAD_DEBT_LOT_PREMIUM_DENOMINATOR: i128 = 5;
const BAD_DEBT_TIER_MINIMUM_VALUE_USDC: i128 = 100 * SCALAR_7;

/// Canonical single-tier lot selected for one pool's bad-debt auction.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct BadDebtLotQuote {
    /// USDC value of the remaining selected assets after any partial fill.
    pub committed_value: i128,
    /// Oracle-valued bad debt supplied at auction creation.
    pub debt_value: i128,
    pub tier: BackstopTier,
    /// Remaining base token units selected from the tier.
    pub lot_amount: i128,
    /// Creation-time 120% target not filled by available backstop capital.
    pub unfilled_target_value: i128,
    /// Creation-time inherited 120% target lot value.
    pub target_value: i128,
    /// Creation-time earliest expiry of any LP valuation used by the quote.
    pub valid_until: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub(crate) struct BadDebtCommitment {
    pub auction_id: BytesN<32>,
    pub quote: BadDebtLotQuote,
}

#[derive(Clone)]
#[contracttype]
enum BadDebtDataKey {
    Commitment(Address),
}

/// Quote the first qualifying tier in the immutable loss waterfall.
pub(crate) fn quote_bad_debt_lot(
    e: &Env,
    pool: &Address,
    debt_value: i128,
) -> Option<BadDebtLotQuote> {
    require_registered_pool(e, pool);
    build_bad_debt_lot_quote(e, pool, debt_value)
}

/// Reserve one pool-authorized single-tier lot without moving assets.
pub(crate) fn commit_bad_debt_lot(
    e: &Env,
    pool: &Address,
    auction_id: &BytesN<32>,
    debt_value: i128,
) -> BadDebtLotQuote {
    require_registered_pool(e, pool);
    if get_bad_debt_commitment(e, pool).is_some() {
        panic_with_error!(e, BackstopError::BadDebtCommitmentExists);
    }

    let quote = build_bad_debt_lot_quote(e, pool, debt_value)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::NoBadDebtLossCapacity));
    set_bad_debt_commitment(
        e,
        pool,
        &BadDebtCommitment {
            auction_id: auction_id.clone(),
            quote: quote.clone(),
        },
    );
    quote
}

/// Release a pool-authorized commitment without changing tier accounting.
pub(crate) fn release_bad_debt_lot(e: &Env, pool: &Address, auction_id: &BytesN<32>) {
    require_registered_pool(e, pool);
    let commitment = get_bad_debt_commitment(e, pool)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::BadDebtCommitmentNotFound));
    if commitment.auction_id != *auction_id {
        panic_with_error!(e, BackstopError::BadDebtCommitmentNotFound);
    }
    remove_bad_debt_commitment(e, pool);
}

/// Settle one partial or complete fill and transfer the realized tier loss.
pub(crate) fn settle_bad_debt_lot(
    e: &Env,
    pool: &Address,
    auction_id: &BytesN<32>,
    base_lot_amount: i128,
    lot_amount: i128,
    to: &Address,
) -> Option<BadDebtLotQuote> {
    require_registered_pool(e, pool);
    if base_lot_amount < 0 || lot_amount < 0 {
        panic_with_error!(e, BackstopError::NegativeAmountError);
    }
    if lot_amount > base_lot_amount {
        panic_with_error!(e, BackstopError::BadRequest);
    }

    let mut commitment = get_bad_debt_commitment(e, pool)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::BadDebtCommitmentNotFound));
    if commitment.auction_id != *auction_id || base_lot_amount > commitment.quote.lot_amount {
        panic_with_error!(e, BackstopError::BadDebtCommitmentNotFound);
    }

    let tier = commitment.quote.tier;
    let committed_lot_amount = commitment.quote.lot_amount;
    apply_pool_tier_loss(e, tier, pool, lot_amount);
    if lot_amount > 0 {
        TokenClient::new(e, &tier_token(e, tier)).transfer(
            &e.current_contract_address(),
            to,
            &lot_amount,
        );
    }

    let remaining_lot_amount = committed_lot_amount
        .checked_sub(base_lot_amount)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    if remaining_lot_amount == 0 {
        remove_bad_debt_commitment(e, pool);
        None
    } else {
        commitment.quote.committed_value = proportional_floor(
            e,
            commitment.quote.committed_value,
            remaining_lot_amount,
            committed_lot_amount,
        );
        commitment.quote.lot_amount = remaining_lot_amount;
        set_bad_debt_commitment(e, pool, &commitment);
        Some(commitment.quote)
    }
}

pub(crate) fn bad_debt_commitment(
    e: &Env,
    pool: &Address,
    auction_id: &BytesN<32>,
) -> Option<BadDebtLotQuote> {
    get_bad_debt_commitment(e, pool).and_then(|commitment| {
        if commitment.auction_id == *auction_id {
            Some(commitment.quote)
        } else {
            None
        }
    })
}

pub(crate) fn pool_bad_debt_commitment_count(e: &Env, pool: &Address) -> u32 {
    u32::from(get_bad_debt_commitment(e, pool).is_some())
}

pub(crate) fn pool_tier_committed_assets(e: &Env, tier: BackstopTier, pool: &Address) -> i128 {
    get_bad_debt_commitment(e, pool)
        .map(|commitment| {
            if commitment.quote.tier == tier {
                commitment.quote.lot_amount
            } else {
                0
            }
        })
        .unwrap_or(0)
}

pub(crate) fn available_pool_tier_assets(e: &Env, tier: BackstopTier, pool: &Address) -> i128 {
    let assets = storage::get_pool_balance_for_tier(e, tier, pool).tokens;
    let committed = pool_tier_committed_assets(e, tier, pool);
    assets
        .checked_sub(committed)
        .filter(|available| *available >= 0)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::InternalError))
}

fn build_bad_debt_lot_quote(e: &Env, pool: &Address, debt_value: i128) -> Option<BadDebtLotQuote> {
    if debt_value <= 0 {
        panic_with_error!(e, BackstopError::BadRequest);
    }
    let target_value = proportional_ceil(
        e,
        debt_value,
        BAD_DEBT_LOT_PREMIUM_NUMERATOR,
        BAD_DEBT_LOT_PREMIUM_DENOMINATOR,
    );
    for tier in [
        BackstopTier::BlndXlm,
        BackstopTier::BlndUsdc,
        BackstopTier::Usdc,
    ] {
        let assets = available_pool_tier_assets(e, tier, pool);
        if assets == 0 {
            continue;
        }
        let valuation = if tier == BackstopTier::Usdc {
            AssetValuation {
                underlying_blnd: 0,
                usdc_value: assets,
                valid_until: u64::MAX,
            }
        } else {
            quote_lp_amount(e, tier, assets)
        };
        if valuation.usdc_value < BAD_DEBT_TIER_MINIMUM_VALUE_USDC {
            continue;
        }

        let (lot_amount, committed_value, unfilled_target_value) =
            allocate_bad_debt_tier(e, assets, valuation.usdc_value, target_value);
        return Some(BadDebtLotQuote {
            committed_value,
            debt_value,
            tier,
            lot_amount,
            unfilled_target_value,
            target_value,
            valid_until: valuation.valid_until,
        });
    }
    None
}

fn allocate_bad_debt_tier(
    e: &Env,
    available_assets: i128,
    available_value: i128,
    target_value: i128,
) -> (i128, i128, i128) {
    if available_assets <= 0 || available_value <= 0 || target_value <= 0 {
        panic_with_error!(e, BackstopError::InternalError);
    }
    if available_value <= target_value {
        return (
            available_assets,
            available_value,
            target_value
                .checked_sub(available_value)
                .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError)),
        );
    }

    let lot_amount = proportional_ceil(e, target_value, available_assets, available_value);
    if lot_amount <= 0 || lot_amount > available_assets {
        panic_with_error!(e, BackstopError::InternalError);
    }
    let committed_value = proportional_floor(e, available_value, lot_amount, available_assets);
    if committed_value < target_value {
        panic_with_error!(e, BackstopError::InternalError);
    }
    (lot_amount, committed_value, 0)
}

fn get_bad_debt_commitment(e: &Env, pool: &Address) -> Option<BadDebtCommitment> {
    e.storage()
        .temporary()
        .get(&BadDebtDataKey::Commitment(pool.clone()))
}

fn set_bad_debt_commitment(e: &Env, pool: &Address, commitment: &BadDebtCommitment) {
    let key = BadDebtDataKey::Commitment(pool.clone());
    e.storage().temporary().set(&key, commitment);
    e.storage()
        .temporary()
        .extend_ttl(&key, AUCTION_TTL_THRESHOLD, AUCTION_TTL_BUMP);
}

fn remove_bad_debt_commitment(e: &Env, pool: &Address) {
    e.storage()
        .temporary()
        .remove(&BadDebtDataKey::Commitment(pool.clone()));
}

fn apply_pool_tier_loss(e: &Env, tier: BackstopTier, pool: &Address, assets: i128) {
    if assets < 0 {
        panic_with_error!(e, BackstopError::NegativeAmountError);
    }
    if assets == 0 {
        return;
    }
    emissions::prepare_pool_weight_change(e, tier, pool);
    let mut balance = storage::get_pool_balance_for_tier(e, tier, pool);
    balance.withdraw(e, assets, 0);
    storage::set_pool_balance_for_tier(e, tier, pool, &balance);
    update_tier_totals(e, tier, -assets, 0, 0);
    emissions::finish_pool_weight_change(e, tier, pool);
}

fn proportional_floor(e: &Env, value: i128, numerator: i128, denominator: i128) -> i128 {
    if value < 0 || numerator < 0 || denominator <= 0 {
        panic_with_error!(e, BackstopError::OverflowError);
    }
    I256::from_i128(e, value)
        .mul(&I256::from_i128(e, numerator))
        .div(&I256::from_i128(e, denominator))
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

fn proportional_ceil(e: &Env, value: i128, numerator: i128, denominator: i128) -> i128 {
    if value < 0 || numerator < 0 || denominator <= 0 {
        panic_with_error!(e, BackstopError::OverflowError);
    }
    let denominator = I256::from_i128(e, denominator);
    I256::from_i128(e, value)
        .mul(&I256::from_i128(e, numerator))
        .add(&denominator)
        .sub(&I256::from_i32(e, 1))
        .div(&denominator)
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

#[cfg(test)]
mod tests {
    use soroban_sdk::{testutils::Address as _, Address, BytesN};

    use crate::{
        backstop::{PoolBalance, PoolValuation, TierTotals},
        storage,
        testutils::{
            create_backstop, create_mock_pool, create_mock_pool_factory, create_usdc_token,
        },
        BackstopClient,
    };

    use super::*;

    fn set_tier_balance(
        e: &Env,
        backstop: &Address,
        pool: &Address,
        tier: BackstopTier,
        amount: i128,
    ) {
        e.as_contract(backstop, || {
            storage::set_pool_balance_for_tier(
                e,
                tier,
                pool,
                &PoolBalance {
                    shares: amount,
                    tokens: amount,
                    q4w: 0,
                },
            );
        });
    }

    #[test]
    fn commits_first_qualifying_single_tier_and_releases_it() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        let backstop = create_backstop(&e);
        let (pool, _) = create_mock_pool(&e, &backstop);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        factory.set_mock_pool(&pool);
        let client = BackstopClient::new(&e, &backstop);
        let blnd_xlm_below_minimum = BAD_DEBT_TIER_MINIMUM_VALUE_USDC - 1;

        set_tier_balance(&e, &backstop, &pool, BackstopTier::BlndUsdc, 50 * SCALAR_7);
        set_tier_balance(
            &e,
            &backstop,
            &pool,
            BackstopTier::BlndXlm,
            blnd_xlm_below_minimum,
        );
        set_tier_balance(&e, &backstop, &pool, BackstopTier::Usdc, 500 * SCALAR_7);

        let debt_value = 200 * SCALAR_7;
        let expected = BadDebtLotQuote {
            committed_value: 240 * SCALAR_7,
            debt_value,
            tier: BackstopTier::Usdc,
            lot_amount: 240 * SCALAR_7,
            unfilled_target_value: 0,
            target_value: 240 * SCALAR_7,
            valid_until: u64::MAX,
        };
        assert_eq!(
            client.quote_bad_debt_lot(&pool, &debt_value),
            Some(expected.clone())
        );

        let auction_id = BytesN::from_array(&e, &[7; 32]);
        assert_eq!(
            client.commit_bad_debt_lot(&pool, &auction_id, &debt_value),
            expected
        );
        assert_eq!(client.pool_bad_debt_commitment_count(&pool), 1);
        assert_eq!(
            client.pool_tier_committed_assets(&BackstopTier::Usdc, &pool),
            240 * SCALAR_7
        );
        assert_eq!(
            client.pool_valuation(&pool),
            PoolValuation {
                active_blnd: super::super::BlndEmissionValues {
                    blnd_usdc: 50 * SCALAR_7,
                    blnd_xlm: blnd_xlm_below_minimum,
                },
                active_values: super::super::ActivationValues {
                    blnd_usdc: 50 * SCALAR_7,
                    blnd_xlm: blnd_xlm_below_minimum,
                    usdc: 260 * SCALAR_7,
                },
                queued_values: super::super::ActivationValues {
                    blnd_usdc: 0,
                    blnd_xlm: 0,
                    usdc: 0,
                },
                valid_until: u64::MAX,
            }
        );

        client.release_bad_debt_lot(&pool, &auction_id);
        assert_eq!(client.pool_bad_debt_commitment_count(&pool), 0);
        assert_eq!(
            client.pool_valuation(&pool).active_values.usdc,
            500 * SCALAR_7
        );
    }

    #[test]
    fn blnd_xlm_is_first_loss_when_both_lp_tiers_qualify() {
        let e = Env::default();
        let backstop = create_backstop(&e);
        let (pool, _) = create_mock_pool(&e, &backstop);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        factory.set_mock_pool(&pool);

        set_tier_balance(
            &e,
            &backstop,
            &pool,
            BackstopTier::BlndUsdc,
            BAD_DEBT_TIER_MINIMUM_VALUE_USDC,
        );
        set_tier_balance(
            &e,
            &backstop,
            &pool,
            BackstopTier::BlndXlm,
            BAD_DEBT_TIER_MINIMUM_VALUE_USDC,
        );

        assert_eq!(
            BackstopClient::new(&e, &backstop)
                .quote_bad_debt_lot(&pool, &SCALAR_7)
                .unwrap()
                .tier,
            BackstopTier::BlndXlm
        );
    }

    #[test]
    fn skips_every_tier_below_the_operational_minimum() {
        let e = Env::default();
        let backstop = create_backstop(&e);
        let (pool, _) = create_mock_pool(&e, &backstop);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        factory.set_mock_pool(&pool);

        for tier in [
            BackstopTier::BlndUsdc,
            BackstopTier::BlndXlm,
            BackstopTier::Usdc,
        ] {
            set_tier_balance(
                &e,
                &backstop,
                &pool,
                tier,
                BAD_DEBT_TIER_MINIMUM_VALUE_USDC - 1,
            );
        }
        assert_eq!(
            BackstopClient::new(&e, &backstop).quote_bad_debt_lot(&pool, &SCALAR_7),
            None
        );

        set_tier_balance(
            &e,
            &backstop,
            &pool,
            BackstopTier::BlndUsdc,
            BAD_DEBT_TIER_MINIMUM_VALUE_USDC,
        );
        assert_eq!(
            BackstopClient::new(&e, &backstop)
                .quote_bad_debt_lot(&pool, &SCALAR_7)
                .unwrap()
                .tier,
            BackstopTier::BlndUsdc
        );
    }

    #[test]
    fn partial_tier_selection_rounds_assets_up() {
        let e = Env::default();
        assert_eq!(allocate_bad_debt_tier(&e, 3, 2, 1), (2, 1, 0));
        assert_eq!(allocate_bad_debt_tier(&e, 3, 2, 2), (3, 2, 0));
        assert_eq!(allocate_bad_debt_tier(&e, 3, 2, 3), (3, 2, 1));
    }

    #[test]
    fn partial_and_complete_settlement_apply_only_realized_tier_loss() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        let backstop = create_backstop(&e);
        let (pool, _) = create_mock_pool(&e, &backstop);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        factory.set_mock_pool(&pool);
        let admin = Address::generate(&e);
        let recipient = Address::generate(&e);
        let (_, usdc) = create_usdc_token(&e, &backstop, &admin);
        let initial_assets = 500 * SCALAR_7;
        usdc.mint(&backstop, &initial_assets);
        set_tier_balance(&e, &backstop, &pool, BackstopTier::Usdc, initial_assets);
        e.as_contract(&backstop, || {
            storage::set_tier_totals(
                &e,
                BackstopTier::Usdc,
                &TierTotals {
                    assets: initial_assets,
                    queued_shares: 0,
                    shares: initial_assets,
                },
            );
        });

        let client = BackstopClient::new(&e, &backstop);
        let auction_id = BytesN::from_array(&e, &[8; 32]);
        let quote = client.commit_bad_debt_lot(&pool, &auction_id, &(200 * SCALAR_7));
        assert_eq!(quote.lot_amount, 240 * SCALAR_7);

        assert_eq!(
            client.settle_bad_debt_lot(
                &pool,
                &auction_id,
                &(120 * SCALAR_7),
                &(60 * SCALAR_7),
                &recipient,
            ),
            Some(BadDebtLotQuote {
                committed_value: 120 * SCALAR_7,
                debt_value: 200 * SCALAR_7,
                tier: BackstopTier::Usdc,
                lot_amount: 120 * SCALAR_7,
                unfilled_target_value: 0,
                target_value: 240 * SCALAR_7,
                valid_until: u64::MAX,
            })
        );
        assert_eq!(usdc.balance(&recipient), 60 * SCALAR_7);
        assert_eq!(
            client.pool_tier_state(&BackstopTier::Usdc, &pool),
            super::super::PoolTierState {
                assets: 440 * SCALAR_7,
                queued_shares: 0,
                shares: initial_assets,
            }
        );
        assert_eq!(
            client.tier_totals(&BackstopTier::Usdc).assets,
            440 * SCALAR_7
        );

        assert!(client
            .try_settle_bad_debt_lot(
                &pool,
                &auction_id,
                &(121 * SCALAR_7),
                &(121 * SCALAR_7),
                &recipient,
            )
            .is_err());
        assert_eq!(usdc.balance(&recipient), 60 * SCALAR_7);
        assert_eq!(
            client
                .bad_debt_commitment(&pool, &auction_id)
                .unwrap()
                .lot_amount,
            120 * SCALAR_7
        );

        assert_eq!(
            client.settle_bad_debt_lot(
                &pool,
                &auction_id,
                &(120 * SCALAR_7),
                &(120 * SCALAR_7),
                &recipient,
            ),
            None
        );
        assert_eq!(usdc.balance(&recipient), 180 * SCALAR_7);
        assert_eq!(
            client.pool_tier_state(&BackstopTier::Usdc, &pool),
            super::super::PoolTierState {
                assets: 320 * SCALAR_7,
                queued_shares: 0,
                shares: initial_assets,
            }
        );
        assert_eq!(client.pool_bad_debt_commitment_count(&pool), 0);
    }
}
