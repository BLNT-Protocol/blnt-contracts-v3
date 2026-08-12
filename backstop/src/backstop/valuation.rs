use sep_41_token::TokenClient;
use soroban_sdk::{contracttype, panic_with_error, Address, Env, I256};

use crate::{
    constants::{ACTIVATION_ENTRY_THRESHOLD_USDC, ACTIVATION_MAINTENANCE_THRESHOLD_USDC, SCALAR_7},
    dependencies::CometClient,
    errors::BackstopError,
    storage,
};

use super::{require_registered_pool, BackstopTier, PoolBalance};

const BLND_WEIGHT: i128 = 8_000_000;
const PAIR_WEIGHT: i128 = 2_000_000;
const PAIR_VALUE_MULTIPLIER: i128 = 5;
const TOKEN_DECIMALS: u32 = 7;

#[derive(Clone, Debug, Eq, PartialEq)]
struct AssetValuation {
    pub underlying_blnd: i128,
    pub usdc_value: i128,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ActivationValues {
    pub blnd_usdc: i128,
    pub blnd_xlm: i128,
    pub usdc: i128,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ActivationQuote {
    pub eligible_value: i128,
    pub meets_threshold: bool,
    pub required_value: i128,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype(export = false)]
pub(crate) struct BlndEmissionValues {
    pub blnd_usdc: i128,
    pub blnd_xlm: i128,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PoolValuation {
    pub active_blnd: BlndEmissionValues,
    pub active_values: ActivationValues,
    pub queued_values: ActivationValues,
    pub total_values: ActivationValues,
}

struct PoolTierValuation {
    active: AssetValuation,
    queued: AssetValuation,
    total: AssetValuation,
}

/// One pool's compact accounting and canonical valuation for a fixed backstop tier.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct PoolTierData {
    /// Total tier-token assets.
    pub tokens: i128,
    /// Total issued shares, including queued shares.
    pub shares: i128,
    /// USDC value of all tier tokens.
    pub value: i128,
}

/// One pool's complete three-tier backstop accounting and valuation.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct PoolBackstopData {
    /// Aggregate USDC value excluding queued withdrawals.
    pub active_value: i128,
    pub blnd_usdc: PoolTierData,
    pub blnd_xlm: PoolTierData,
    /// Queued value divided by total active-plus-queued value, rounded up.
    pub q4w_pct: i128,
    pub usdc: PoolTierData,
}

pub(crate) fn build_pool_data(e: &Env, pool: &Address) -> PoolBackstopData {
    let valuation = build_pool_valuation(e, pool);
    let blnd_usdc = storage::get_pool_balance_for_tier(e, BackstopTier::BlndUsdc, pool);
    let blnd_xlm = storage::get_pool_balance_for_tier(e, BackstopTier::BlndXlm, pool);
    let usdc = storage::get_pool_balance_for_tier(e, BackstopTier::Usdc, pool);

    PoolBackstopData {
        active_value: sum_activation_values(e, &valuation.active_values),
        blnd_usdc: tier_data(&blnd_usdc, valuation.total_values.blnd_usdc),
        blnd_xlm: tier_data(&blnd_xlm, valuation.total_values.blnd_xlm),
        usdc: tier_data(&usdc, valuation.total_values.usdc),
        q4w_pct: calculate_q4w_percentage(e, &valuation.active_values, &valuation.queued_values),
    }
}

fn tier_data(balance: &PoolBalance, value: i128) -> PoolTierData {
    PoolTierData {
        tokens: balance.tokens,
        shares: balance.shares,
        value,
    }
}

pub(crate) fn build_pool_valuation(e: &Env, pool: &Address) -> PoolValuation {
    // A pool invokes this while refreshing its own status. Factory
    // registration is sufficient and avoids a pool -> backstop -> pool cycle.
    require_registered_pool(e, pool);
    let (blnd_usdc_active, blnd_usdc_queued, blnd_usdc_total) =
        pool_tier_asset_partition(e, BackstopTier::BlndUsdc, pool);
    let (blnd_xlm_active, blnd_xlm_queued, blnd_xlm_total) =
        pool_tier_asset_partition(e, BackstopTier::BlndXlm, pool);
    let (usdc_active, usdc_queued, usdc_total) =
        pool_tier_asset_partition(e, BackstopTier::Usdc, pool);

    let (blnd_usdc_quotes, blnd_xlm_quotes) = quote_pool_lp_amounts(
        e,
        (blnd_usdc_active, blnd_usdc_queued, blnd_usdc_total),
        (blnd_xlm_active, blnd_xlm_queued, blnd_xlm_total),
    );

    PoolValuation {
        active_blnd: BlndEmissionValues {
            blnd_usdc: blnd_usdc_quotes.active.underlying_blnd,
            blnd_xlm: blnd_xlm_quotes.active.underlying_blnd,
        },
        active_values: ActivationValues {
            blnd_usdc: blnd_usdc_quotes.active.usdc_value,
            blnd_xlm: blnd_xlm_quotes.active.usdc_value,
            usdc: usdc_active,
        },
        queued_values: ActivationValues {
            blnd_usdc: blnd_usdc_quotes.queued.usdc_value,
            blnd_xlm: blnd_xlm_quotes.queued.usdc_value,
            usdc: usdc_queued,
        },
        total_values: ActivationValues {
            blnd_usdc: blnd_usdc_quotes.total.usdc_value,
            blnd_xlm: blnd_xlm_quotes.total.usdc_value,
            usdc: usdc_total,
        },
    }
}

fn quote_pool_lp_amounts(
    e: &Env,
    blnd_usdc: (i128, i128, i128),
    blnd_xlm: (i128, i128, i128),
) -> (PoolTierValuation, PoolTierValuation) {
    for amount in [
        blnd_usdc.0,
        blnd_usdc.1,
        blnd_usdc.2,
        blnd_xlm.0,
        blnd_xlm.1,
        blnd_xlm.2,
    ] {
        if amount < 0 {
            panic_with_error!(e, BackstopError::InvalidValuation);
        }
    }

    if blnd_usdc.2 == 0 && blnd_xlm.2 == 0 {
        return (
            unit_pool_tier_valuation(blnd_usdc),
            unit_pool_tier_valuation(blnd_xlm),
        );
    }

    #[cfg(any(test, feature = "testutils"))]
    if let Some(should_fail) = test_valuation_override(e) {
        if should_fail {
            panic_with_error!(e, BackstopError::InvalidValuation);
        }
        return (
            unit_pool_tier_valuation(blnd_usdc),
            unit_pool_tier_valuation(blnd_xlm),
        );
    }

    let blnd = storage::get_blnd_token(e);
    let usdc = storage::get_usdc_token(e);
    let anchor = read_comet(e, &storage::get_blnd_usdc_token(e), &blnd, &usdc);
    let anchor_value = checked_mul(e, anchor.pair_reserve, PAIR_VALUE_MULTIPLIER);
    let blnd_usdc_quotes = pool_tier_valuation(e, blnd_usdc, anchor_value, &anchor);
    let blnd_xlm_quotes = if blnd_xlm.2 == 0 {
        unit_pool_tier_valuation(blnd_xlm)
    } else {
        let target = read_comet(
            e,
            &storage::get_blnd_xlm_token(e),
            &blnd,
            &storage::get_xlm_token(e),
        );
        let target_value = mul_div_floor(e, target.blnd_reserve, anchor_value, anchor.blnd_reserve);
        pool_tier_valuation(e, blnd_xlm, target_value, &target)
    };
    (blnd_usdc_quotes, blnd_xlm_quotes)
}

fn pool_tier_valuation(
    e: &Env,
    amounts: (i128, i128, i128),
    total_value: i128,
    composition: &CometComposition,
) -> PoolTierValuation {
    if total_value <= 0 || amounts.2 > composition.total_supply {
        panic_with_error!(e, BackstopError::InvalidValuation);
    }
    PoolTierValuation {
        active: quote_from_composition(e, amounts.0, total_value, composition),
        queued: quote_from_composition(e, amounts.1, total_value, composition),
        total: quote_from_composition(e, amounts.2, total_value, composition),
    }
}

fn quote_from_composition(
    e: &Env,
    amount: i128,
    total_value: i128,
    composition: &CometComposition,
) -> AssetValuation {
    AssetValuation {
        underlying_blnd: mul_div_floor(
            e,
            amount,
            composition.blnd_reserve,
            composition.total_supply,
        ),
        usdc_value: mul_div_floor(e, amount, total_value, composition.total_supply),
    }
}

fn unit_pool_tier_valuation(amounts: (i128, i128, i128)) -> PoolTierValuation {
    PoolTierValuation {
        active: unit_asset_valuation(amounts.0),
        queued: unit_asset_valuation(amounts.1),
        total: unit_asset_valuation(amounts.2),
    }
}

fn unit_asset_valuation(amount: i128) -> AssetValuation {
    AssetValuation {
        underlying_blnd: amount,
        usdc_value: amount,
    }
}

pub fn quote_activation(
    e: &Env,
    values: &ActivationValues,
    currently_active: bool,
) -> ActivationQuote {
    let eligible_value = sum_activation_values(e, values);
    let required_value = if currently_active {
        ACTIVATION_MAINTENANCE_THRESHOLD_USDC
    } else {
        ACTIVATION_ENTRY_THRESHOLD_USDC
    };
    ActivationQuote {
        eligible_value,
        meets_threshold: eligible_value >= required_value,
        required_value,
    }
}

fn pool_tier_asset_partition(e: &Env, tier: BackstopTier, pool: &Address) -> (i128, i128, i128) {
    let balance = storage::get_pool_balance_for_tier(e, tier, pool);
    let active_shares = balance
        .shares
        .checked_sub(balance.q4w)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::InvalidValuation));
    let active = assets_from_shares(e, active_shares, &balance);
    let queued = assets_from_shares(e, balance.q4w, &balance);
    (active, queued, balance.tokens)
}

fn assets_from_shares(e: &Env, shares: i128, balance: &PoolBalance) -> i128 {
    if shares < 0 || balance.tokens < 0 || balance.shares < 0 {
        panic_with_error!(e, BackstopError::InvalidValuation);
    }
    balance.convert_to_tokens(shares)
}

pub(crate) fn validate_backstop_assets(
    e: &Env,
    blnd: &Address,
    usdc: &Address,
    xlm: &Address,
    blnd_usdc: &Address,
    blnd_xlm: &Address,
) {
    let addresses = [blnd, usdc, xlm, blnd_usdc, blnd_xlm];
    for (index, address) in addresses.iter().enumerate() {
        if addresses
            .iter()
            .skip(index + 1)
            .any(|other| address == other)
        {
            panic_with_error!(e, BackstopError::AssetConfigurationCollision);
        }
    }

    for token in addresses {
        if TokenClient::new(e, token).decimals() != TOKEN_DECIMALS {
            panic_with_error!(e, BackstopError::InvalidBackstopValuation);
        }
    }
    validate_comet(e, blnd_usdc, blnd, usdc);
    validate_comet(e, blnd_xlm, blnd, xlm);
}

struct CometComposition {
    blnd_reserve: i128,
    pair_reserve: i128,
    total_supply: i128,
}

fn validate_comet(e: &Env, comet: &Address, blnd: &Address, pair: &Address) {
    let client = CometClient::new(e, comet);
    let tokens = client.get_tokens();
    if tokens.len() != 2
        || !tokens.contains(blnd)
        || !tokens.contains(pair)
        || client.get_normalized_weight(blnd) != BLND_WEIGHT
        || client.get_normalized_weight(pair) != PAIR_WEIGHT
    {
        panic_with_error!(e, BackstopError::InvalidBackstopValuation);
    }
}

fn read_comet(e: &Env, comet: &Address, blnd: &Address, pair: &Address) -> CometComposition {
    let client = CometClient::new(e, comet);
    let total_supply = client.get_total_supply();
    let blnd_reserve = client.get_balance(blnd);
    let pair_reserve = client.get_balance(pair);
    if total_supply <= 0
        || blnd_reserve <= 0
        || pair_reserve <= 0
        || client.get_normalized_weight(blnd) != BLND_WEIGHT
        || client.get_normalized_weight(pair) != PAIR_WEIGHT
    {
        panic_with_error!(e, BackstopError::InvalidValuation);
    }
    CometComposition {
        blnd_reserve,
        pair_reserve,
        total_supply,
    }
}

fn checked_mul(e: &Env, left: i128, right: i128) -> i128 {
    left.checked_mul(right)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

fn mul_div_floor(e: &Env, value: i128, numerator: i128, denominator: i128) -> i128 {
    if value < 0 || numerator < 0 || denominator <= 0 {
        panic_with_error!(e, BackstopError::InvalidValuation);
    }
    I256::from_i128(e, value)
        .mul(&I256::from_i128(e, numerator))
        .div(&I256::from_i128(e, denominator))
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

#[cfg(any(test, feature = "testutils"))]
#[derive(Clone)]
#[contracttype]
enum TestValuationKey {
    Override,
}

#[cfg(any(test, feature = "testutils"))]
fn test_valuation_override(e: &Env) -> Option<bool> {
    e.storage().instance().get(&TestValuationKey::Override)
}

#[cfg(any(test, feature = "testutils"))]
pub fn set_test_valuation_override(e: &Env, should_fail: Option<bool>) {
    if let Some(should_fail) = should_fail {
        e.storage()
            .instance()
            .set(&TestValuationKey::Override, &should_fail);
    } else {
        e.storage().instance().remove(&TestValuationKey::Override);
    }
}

fn sum_activation_values(e: &Env, values: &ActivationValues) -> i128 {
    if values.blnd_usdc < 0 || values.blnd_xlm < 0 || values.usdc < 0 {
        panic_with_error!(e, BackstopError::InvalidActivationValue);
    }
    values
        .blnd_usdc
        .checked_add(values.blnd_xlm)
        .and_then(|value| value.checked_add(values.usdc))
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

fn calculate_q4w_percentage(
    e: &Env,
    active_values: &ActivationValues,
    queued_values: &ActivationValues,
) -> i128 {
    let active_value = sum_activation_values(e, active_values);
    let queued_value = sum_activation_values(e, queued_values);
    let total_value = active_value
        .checked_add(queued_value)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    if total_value == 0 {
        return 0;
    }

    let numerator = I256::from_i128(e, queued_value).mul(&I256::from_i128(e, SCALAR_7));
    let denominator = I256::from_i128(e, total_value);
    numerator
        .add(&denominator)
        .sub(&I256::from_i32(e, 1))
        .div(&denominator)
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

#[cfg(test)]
mod tests {
    use soroban_sdk::{testutils::Address as _, Address};

    use crate::{
        testutils::{
            create_backstop, create_backstop_token, create_backstop_with_real_comets,
            create_blnd_xlm_token, create_mock_pool_factory, create_usdc_token,
        },
        BackstopClient,
    };

    use super::*;

    fn values(blnd_usdc: i128, blnd_xlm: i128, usdc: i128) -> ActivationValues {
        ActivationValues {
            blnd_usdc,
            blnd_xlm,
            usdc,
        }
    }

    #[test]
    fn activation_values_all_tiers_equally_and_applies_hysteresis() {
        let e = Env::default();
        let entry = values(4_000 * SCALAR_7, 3_500 * SCALAR_7, 5_000 * SCALAR_7);
        assert_eq!(
            quote_activation(&e, &entry, false),
            ActivationQuote {
                eligible_value: ACTIVATION_ENTRY_THRESHOLD_USDC,
                meets_threshold: true,
                required_value: ACTIVATION_ENTRY_THRESHOLD_USDC,
            }
        );

        let maintenance = values(0, 0, ACTIVATION_MAINTENANCE_THRESHOLD_USDC);
        assert!(quote_activation(&e, &maintenance, true).meets_threshold);
        assert!(!quote_activation(&e, &maintenance, false).meets_threshold);

        let below_maintenance = values(0, 0, ACTIVATION_MAINTENANCE_THRESHOLD_USDC - 1);
        assert!(!quote_activation(&e, &below_maintenance, true).meets_threshold);
    }

    #[test]
    fn integrated_comet_valuation_preserves_reserve_implied_formulas() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited();
        let backstop = create_backstop_with_real_comets(&e);

        let (blnd_usdc, blnd_xlm) = e.as_contract(&backstop, || {
            set_test_valuation_override(&e, None);
            let (blnd_usdc, blnd_xlm) = quote_pool_lp_amounts(
                &e,
                (20 * SCALAR_7, 0, 20 * SCALAR_7),
                (10 * SCALAR_7, 0, 10 * SCALAR_7),
            );
            (blnd_usdc.total, blnd_xlm.total)
        });

        assert_eq!(
            blnd_usdc,
            AssetValuation {
                underlying_blnd: 200 * SCALAR_7,
                usdc_value: 25 * SCALAR_7,
            }
        );
        assert_eq!(
            blnd_xlm,
            AssetValuation {
                underlying_blnd: 100 * SCALAR_7,
                usdc_value: 125_000_000,
            }
        );
    }

    #[test]
    fn q4w_percentage_uses_value_across_tiers_and_rounds_up() {
        let e = Env::default();
        let active = values(4_000 * SCALAR_7, 3_000 * SCALAR_7, 0);
        let queued = values(0, 0, 3_000 * SCALAR_7);
        assert_eq!(calculate_q4w_percentage(&e, &active, &queued), 3_000_000);

        let rounded =
            calculate_q4w_percentage(&e, &values(0, 0, 2 * SCALAR_7), &values(0, 0, SCALAR_7));
        assert_eq!(rounded, 3_333_334);
    }

    #[test]
    fn canonical_pool_data_combines_tier_accounting_and_valuation() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&e);
        let user = Address::generate(&e);
        let pool = Address::generate(&e);
        let backstop = create_backstop(&e);
        let (_, blnd_usdc) = create_backstop_token(&e, &backstop, &admin);
        let (_, blnd_xlm) = create_blnd_xlm_token(&e, &backstop, &admin);
        let (_, usdc) = create_usdc_token(&e, &backstop, &admin);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        factory.set_mock_pool(&pool);

        blnd_usdc.mint(&user, &(4_000 * SCALAR_7));
        blnd_xlm.mint(&user, &(5_000 * SCALAR_7));
        usdc.mint(&user, &(6_500 * SCALAR_7));
        let backstop_client = BackstopClient::new(&e, &backstop);
        backstop_client.deposit(
            &crate::BackstopTier::BlndUsdc,
            &user,
            &pool,
            &(4_000 * SCALAR_7),
        );
        backstop_client.deposit(
            &crate::BackstopTier::BlndXlm,
            &user,
            &pool,
            &(5_000 * SCALAR_7),
        );
        backstop_client.deposit(
            &crate::BackstopTier::Usdc,
            &user,
            &pool,
            &(6_500 * SCALAR_7),
        );
        backstop_client.queue_withdrawal(
            &crate::BackstopTier::BlndUsdc,
            &user,
            &pool,
            &(1_000 * SCALAR_7),
        );
        backstop_client.queue_withdrawal(
            &crate::BackstopTier::BlndXlm,
            &user,
            &pool,
            &(2_000 * SCALAR_7),
        );

        let pool_data = backstop_client.pool_data(&pool);
        assert_eq!(
            pool_data.blnd_usdc,
            PoolTierData {
                tokens: 4_000 * SCALAR_7,
                shares: 4_000 * SCALAR_7,
                value: 4_000 * SCALAR_7,
            }
        );
        assert_eq!(
            pool_data.blnd_xlm,
            PoolTierData {
                tokens: 5_000 * SCALAR_7,
                shares: 5_000 * SCALAR_7,
                value: 5_000 * SCALAR_7,
            }
        );
        assert_eq!(
            pool_data.usdc,
            PoolTierData {
                tokens: 6_500 * SCALAR_7,
                shares: 6_500 * SCALAR_7,
                value: 6_500 * SCALAR_7,
            }
        );
        assert_eq!(pool_data.active_value, ACTIVATION_ENTRY_THRESHOLD_USDC);
        assert_eq!(pool_data.q4w_pct, 1_935_484);
        let quote = e.as_contract(&backstop, || {
            let valuation = build_pool_valuation(&e, &pool);
            quote_activation(&e, &valuation.active_values, false)
        });
        assert_eq!(quote.eligible_value, ACTIVATION_ENTRY_THRESHOLD_USDC);
        assert!(quote.meets_threshold);
    }
}
