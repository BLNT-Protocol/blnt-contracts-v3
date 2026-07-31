use sep_41_token::TokenClient;
use soroban_sdk::{contracttype, panic_with_error, Address, Env, I256};

use crate::{
    constants::{ACTIVATION_ENTRY_THRESHOLD_USDC, ACTIVATION_MAINTENANCE_THRESHOLD_USDC, SCALAR_7},
    dependencies::CometClient,
    errors::BackstopError,
    storage,
};

use super::{available_pool_tier_assets, require_registered_pool, BackstopTier, PoolBalance};

const STATUS_ADMIN_ACTIVE: u32 = 0;
const STATUS_ACTIVE: u32 = 1;
const STATUS_ADMIN_ON_ICE: u32 = 2;
const STATUS_ON_ICE: u32 = 3;
const STATUS_ADMIN_FROZEN: u32 = 4;
const STATUS_FROZEN: u32 = 5;
const STATUS_SETUP: u32 = 6;

const Q4W_ON_ICE_THRESHOLD: i128 = 3_000_000;
const Q4W_ADMIN_ACTIVE_LIMIT: i128 = 5_000_000;
const Q4W_FROZEN_THRESHOLD: i128 = 6_000_000;
const Q4W_ADMIN_ON_ICE_LIMIT: i128 = 7_500_000;
const BLND_WEIGHT: i128 = 8_000_000;
const PAIR_WEIGHT: i128 = 2_000_000;
const PAIR_VALUE_MULTIPLIER: i128 = 5;
const TOKEN_DECIMALS: u32 = 7;

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct AssetValuation {
    pub underlying_blnd: i128,
    pub usdc_value: i128,
    pub valid_until: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct ActivationValues {
    pub blnd_usdc: i128,
    pub blnd_xlm: i128,
    pub usdc: i128,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct ActivationQuote {
    pub eligible_value: i128,
    pub meets_threshold: bool,
    pub required_value: i128,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct BlndEmissionValues {
    pub blnd_usdc: i128,
    pub blnd_xlm: i128,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct PoolValuation {
    pub active_blnd: BlndEmissionValues,
    pub active_values: ActivationValues,
    pub queued_values: ActivationValues,
    pub valid_until: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct PoolStatusQuote {
    pub eligible_value: i128,
    pub meets_activation_threshold: bool,
    pub q4w_percentage: i128,
    pub required_value: i128,
    pub status: u32,
    pub transition_allowed: bool,
}

pub fn build_pool_valuation(e: &Env, pool: &Address) -> PoolValuation {
    // A pool invokes this while refreshing its own status. Factory
    // registration is sufficient and avoids a pool -> backstop -> pool cycle.
    require_registered_pool(e, pool);
    let (blnd_usdc_active, blnd_usdc_queued) =
        pool_tier_asset_partition(e, BackstopTier::BlndUsdc, pool);
    let (blnd_xlm_active, blnd_xlm_queued) =
        pool_tier_asset_partition(e, BackstopTier::BlndXlm, pool);
    let (usdc_active, usdc_queued) = pool_tier_asset_partition(e, BackstopTier::Usdc, pool);

    let blnd_usdc_active_quote = quote_lp_amount(e, BackstopTier::BlndUsdc, blnd_usdc_active);
    let blnd_usdc_queued_quote = quote_lp_amount(e, BackstopTier::BlndUsdc, blnd_usdc_queued);
    let blnd_xlm_active_quote = quote_lp_amount(e, BackstopTier::BlndXlm, blnd_xlm_active);
    let blnd_xlm_queued_quote = quote_lp_amount(e, BackstopTier::BlndXlm, blnd_xlm_queued);

    PoolValuation {
        active_blnd: BlndEmissionValues {
            blnd_usdc: blnd_usdc_active_quote.underlying_blnd,
            blnd_xlm: blnd_xlm_active_quote.underlying_blnd,
        },
        active_values: ActivationValues {
            blnd_usdc: blnd_usdc_active_quote.usdc_value,
            blnd_xlm: blnd_xlm_active_quote.usdc_value,
            usdc: usdc_active,
        },
        queued_values: ActivationValues {
            blnd_usdc: blnd_usdc_queued_quote.usdc_value,
            blnd_xlm: blnd_xlm_queued_quote.usdc_value,
            usdc: usdc_queued,
        },
        valid_until: blnd_usdc_active_quote
            .valid_until
            .min(blnd_usdc_queued_quote.valid_until)
            .min(blnd_xlm_active_quote.valid_until)
            .min(blnd_xlm_queued_quote.valid_until),
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

pub fn quote_status_update(
    e: &Env,
    current_status: u32,
    active_values: &ActivationValues,
    queued_values: &ActivationValues,
) -> PoolStatusQuote {
    require_valid_pool_status(e, current_status);
    let currently_active = current_status == STATUS_ADMIN_ACTIVE || current_status == STATUS_ACTIVE;
    let activation = quote_activation(e, active_values, currently_active);
    let q4w_percentage = calculate_q4w_percentage(e, active_values, queued_values);

    let (status, transition_allowed) = match current_status {
        STATUS_SETUP | STATUS_ADMIN_FROZEN => (current_status, false),
        STATUS_ADMIN_ON_ICE => {
            if q4w_percentage >= Q4W_ADMIN_ON_ICE_LIMIT {
                (STATUS_FROZEN, true)
            } else {
                (STATUS_ADMIN_ON_ICE, true)
            }
        }
        STATUS_ADMIN_ACTIVE => {
            if !activation.meets_threshold || q4w_percentage >= Q4W_ADMIN_ACTIVE_LIMIT {
                (STATUS_ON_ICE, true)
            } else {
                (STATUS_ADMIN_ACTIVE, true)
            }
        }
        STATUS_ACTIVE | STATUS_ON_ICE | STATUS_FROZEN => {
            if q4w_percentage >= Q4W_FROZEN_THRESHOLD {
                (STATUS_FROZEN, true)
            } else if !activation.meets_threshold || q4w_percentage >= Q4W_ON_ICE_THRESHOLD {
                (STATUS_ON_ICE, true)
            } else {
                (STATUS_ACTIVE, true)
            }
        }
        _ => panic_with_error!(e, BackstopError::InvalidPoolStatus),
    };

    PoolStatusQuote {
        eligible_value: activation.eligible_value,
        meets_activation_threshold: activation.meets_threshold,
        q4w_percentage,
        required_value: activation.required_value,
        status,
        transition_allowed,
    }
}

pub fn quote_status_set(
    e: &Env,
    current_status: u32,
    requested_status: u32,
    active_values: &ActivationValues,
    queued_values: &ActivationValues,
) -> PoolStatusQuote {
    require_valid_pool_status(e, current_status);
    let currently_active = current_status == STATUS_ADMIN_ACTIVE || current_status == STATUS_ACTIVE;
    let activation = quote_activation(e, active_values, currently_active);
    let q4w_percentage = calculate_q4w_percentage(e, active_values, queued_values);
    let transition_allowed = match requested_status {
        STATUS_ADMIN_ACTIVE => {
            activation.meets_threshold && q4w_percentage < Q4W_ADMIN_ACTIVE_LIMIT
        }
        STATUS_ADMIN_ON_ICE | STATUS_ON_ICE => q4w_percentage < Q4W_ADMIN_ON_ICE_LIMIT,
        STATUS_ADMIN_FROZEN => true,
        _ => panic_with_error!(e, BackstopError::InvalidPoolStatus),
    };

    PoolStatusQuote {
        eligible_value: activation.eligible_value,
        meets_activation_threshold: activation.meets_threshold,
        q4w_percentage,
        required_value: activation.required_value,
        status: if transition_allowed {
            requested_status
        } else {
            current_status
        },
        transition_allowed,
    }
}

fn pool_tier_asset_partition(e: &Env, tier: BackstopTier, pool: &Address) -> (i128, i128) {
    let mut balance = storage::get_pool_balance_for_tier(e, tier, pool);
    balance.tokens = available_pool_tier_assets(e, tier, pool);
    let active_shares = balance
        .shares
        .checked_sub(balance.q4w)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::InvalidValuation));
    (
        assets_from_shares(e, active_shares, &balance),
        assets_from_shares(e, balance.q4w, &balance),
    )
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

pub(crate) fn quote_lp_amount(e: &Env, tier: BackstopTier, amount: i128) -> AssetValuation {
    if amount < 0 {
        panic_with_error!(e, BackstopError::InvalidValuation);
    }
    if amount == 0 {
        return AssetValuation {
            underlying_blnd: 0,
            usdc_value: 0,
            valid_until: u64::MAX,
        };
    }

    #[cfg(any(test, feature = "testutils"))]
    if let Some(should_fail) = test_valuation_override(e) {
        if should_fail {
            panic_with_error!(e, BackstopError::InvalidValuation);
        }
        return AssetValuation {
            underlying_blnd: amount,
            usdc_value: amount,
            valid_until: u64::MAX,
        };
    }

    let blnd = storage::get_blnd_token(e);
    let usdc = storage::get_usdc_token(e);
    let anchor = read_comet(e, &storage::get_blnd_usdc_token(e), &blnd, &usdc);
    let (total_value, composition) = match tier {
        BackstopTier::BlndUsdc => (
            checked_mul(e, anchor.pair_reserve, PAIR_VALUE_MULTIPLIER),
            anchor,
        ),
        BackstopTier::BlndXlm => {
            let target = read_comet(
                e,
                &storage::get_blnd_xlm_token(e),
                &blnd,
                &storage::get_xlm_token(e),
            );
            let anchor_value = checked_mul(e, anchor.pair_reserve, PAIR_VALUE_MULTIPLIER);
            let total_value =
                mul_div_floor(e, target.blnd_reserve, anchor_value, anchor.blnd_reserve);
            (total_value, target)
        }
        BackstopTier::Usdc => panic_with_error!(e, BackstopError::InvalidValuation),
    };
    if amount > composition.total_supply || total_value <= 0 {
        panic_with_error!(e, BackstopError::InvalidValuation);
    }
    AssetValuation {
        underlying_blnd: mul_div_floor(
            e,
            amount,
            composition.blnd_reserve,
            composition.total_supply,
        ),
        usdc_value: mul_div_floor(e, amount, total_value, composition.total_supply),
        valid_until: u64::MAX,
    }
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

fn require_valid_pool_status(e: &Env, status: u32) {
    if status > STATUS_SETUP {
        panic_with_error!(e, BackstopError::InvalidPoolStatus);
    }
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
            (
                quote_lp_amount(&e, BackstopTier::BlndUsdc, 20 * SCALAR_7),
                quote_lp_amount(&e, BackstopTier::BlndXlm, 10 * SCALAR_7),
            )
        });

        assert_eq!(
            blnd_usdc,
            AssetValuation {
                underlying_blnd: 200 * SCALAR_7,
                usdc_value: 25 * SCALAR_7,
                valid_until: u64::MAX,
            }
        );
        assert_eq!(
            blnd_xlm,
            AssetValuation {
                underlying_blnd: 100 * SCALAR_7,
                usdc_value: 125_000_000,
                valid_until: u64::MAX,
            }
        );
    }

    #[test]
    fn status_refresh_uses_maintenance_only_while_active() {
        let e = Env::default();
        let none_queued = values(0, 0, 0);
        let maintenance = values(0, 0, ACTIVATION_MAINTENANCE_THRESHOLD_USDC);

        let active = quote_status_update(&e, STATUS_ACTIVE, &maintenance, &none_queued);
        assert_eq!(active.status, STATUS_ACTIVE);
        assert_eq!(active.required_value, ACTIVATION_MAINTENANCE_THRESHOLD_USDC);

        let inactive = quote_status_update(&e, STATUS_ON_ICE, &maintenance, &none_queued);
        assert_eq!(inactive.status, STATUS_ON_ICE);
        assert_eq!(inactive.required_value, ACTIVATION_ENTRY_THRESHOLD_USDC);

        let entry = values(0, 0, ACTIVATION_ENTRY_THRESHOLD_USDC);
        assert_eq!(
            quote_status_update(&e, STATUS_ON_ICE, &entry, &none_queued).status,
            STATUS_ACTIVE
        );
    }

    #[test]
    fn q4w_status_uses_value_across_tiers_and_rounds_up() {
        let e = Env::default();
        let active = values(4_000 * SCALAR_7, 3_000 * SCALAR_7, 0);
        let queued = values(0, 0, 3_000 * SCALAR_7);
        let quote = quote_status_update(&e, STATUS_ACTIVE, &active, &queued);
        assert_eq!(quote.q4w_percentage, Q4W_ON_ICE_THRESHOLD);
        assert_eq!(quote.status, STATUS_ON_ICE);

        let rounded = quote_status_update(
            &e,
            STATUS_ACTIVE,
            &values(0, 0, 2 * SCALAR_7),
            &values(0, 0, SCALAR_7),
        );
        assert_eq!(rounded.q4w_percentage, 3_333_334);
    }

    #[test]
    fn canonical_pool_valuation_partitions_accounted_tier_assets() {
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

        let valuation = backstop_client.pool_valuation(&pool);
        assert_eq!(
            valuation.active_values,
            values(3_000 * SCALAR_7, 3_000 * SCALAR_7, 6_500 * SCALAR_7)
        );
        assert_eq!(
            valuation.queued_values,
            values(1_000 * SCALAR_7, 2_000 * SCALAR_7, 0)
        );
        assert_eq!(
            valuation.active_blnd,
            BlndEmissionValues {
                blnd_usdc: 3_000 * SCALAR_7,
                blnd_xlm: 3_000 * SCALAR_7,
            }
        );
        assert_eq!(valuation.valid_until, u64::MAX);

        let quote = backstop_client.quote_pool_activation(&pool, &false);
        assert_eq!(quote.eligible_value, ACTIVATION_ENTRY_THRESHOLD_USDC);
        assert!(quote.meets_threshold);
    }
}
