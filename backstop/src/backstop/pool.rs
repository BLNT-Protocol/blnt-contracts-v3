use sep_41_token::TokenClient;
use soroban_fixed_point_math::FixedPoint;
use soroban_sdk::{contracttype, panic_with_error, unwrap::UnwrapOptimized, Address, Env, I256};

use crate::{
    constants::{ACTIVATION_ENTRY_THRESHOLD_USDC, ACTIVATION_MAINTENANCE_THRESHOLD_USDC, SCALAR_7},
    dependencies::{CometClient, PoolFactoryClient},
    errors::BackstopError,
    storage,
};

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

/// One tier's compact accounting and canonical valuation.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct PoolTierData {
    pub tokens: i128,
    pub shares: i128,
    pub value: i128,
}

/// The fixed v3 backstop asset identifiers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[contracttype]
pub enum BackstopTier {
    BlndUsdc,
    BlndXlm,
    Usdc,
}

pub fn tier_token(e: &Env, tier: BackstopTier) -> Address {
    match tier {
        BackstopTier::BlndUsdc => storage::get_blnd_usdc_token(e),
        BackstopTier::BlndXlm => storage::get_blnd_xlm_token(e),
        BackstopTier::Usdc => storage::get_usdc_token(e),
    }
}

pub(crate) fn load_pool_backstop_data(e: &Env, pool: &Address) -> PoolBackstopData {
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

/// Verify the pool address was deployed by the Pool Factory.
///
/// If the pool has an outstanding balance, it is assumed that it was verified before.
///
/// ### Arguments
/// * `address` - The pool address to verify
/// * `balance` - The balance of the pool. A balance of 0 indicates the pool has not been initialized.
///
/// ### Panics
/// If the pool address cannot be verified
pub fn require_is_from_pool_factory(e: &Env, address: &Address, balance: i128) {
    if balance == 0 {
        let pool_factory_client = PoolFactoryClient::new(e, &storage::get_pool_factory(e));
        if !pool_factory_client.is_pool(address) {
            panic_with_error!(e, BackstopError::NotPool);
        }
    }
}

/// Verify a pool is registered by the configured pool factory.
pub fn require_registered_pool(e: &Env, address: &Address) {
    let pool_factory_client = PoolFactoryClient::new(e, &storage::get_pool_factory(e));
    if !pool_factory_client.is_pool(address) {
        panic_with_error!(e, BackstopError::NotPool);
    }
}

/// The pool's backstop balances
#[derive(Clone)]
#[contracttype(export = false)]
pub struct PoolBalance {
    pub shares: i128, // the amount of shares the pool has issued
    pub tokens: i128, // the number of tokens the pool holds in the backstop
    pub q4w: i128,    // the number of shares queued for withdrawal
}

impl PoolBalance {
    /// Convert a token balance to a share balance based on the current pool state
    ///
    /// ### Arguments
    /// * `tokens` - the token balance to convert
    pub fn convert_to_shares(&self, tokens: i128) -> i128 {
        if self.shares == 0 {
            return tokens;
        }
        if self.tokens == 0 {
            return 0;
        }

        tokens
            .fixed_mul_floor(self.shares, self.tokens)
            .unwrap_optimized()
    }

    /// Convert a pool share balance to a token balance based on the current pool state
    ///
    /// ### Arguments
    /// * `shares` - the pool share balance to convert
    pub fn convert_to_tokens(&self, shares: i128) -> i128 {
        if self.shares == 0 {
            return 0;
        }
        if self.shares == shares {
            return self.tokens;
        }

        shares
            .fixed_mul_floor(self.tokens, self.shares)
            .unwrap_optimized()
    }

    /// Determine the amount of effective tokens (not queued for withdrawal) in the pool
    pub fn non_queued_tokens(&self) -> i128 {
        self.tokens - self.convert_to_tokens(self.q4w)
    }

    /// Deposit tokens and shares into the pool
    ///
    /// ### Arguments
    /// * `tokens` - The amount of tokens to add
    /// * `shares` - The amount of shares to add
    pub fn deposit(&mut self, tokens: i128, shares: i128) {
        self.tokens += tokens;
        self.shares += shares;
    }

    /// Withdraw tokens and shares from the pool
    ///
    /// ### Arguments
    /// * `tokens` - The amount of tokens to withdraw
    /// * `shares` - The amount of shares to withdraw
    pub fn withdraw(&mut self, e: &Env, tokens: i128, shares: i128) {
        if tokens > self.tokens || shares > self.shares || shares > self.q4w {
            panic_with_error!(e, BackstopError::InsufficientFunds);
        }
        self.tokens -= tokens;
        self.shares -= shares;
        self.q4w -= shares;
    }

    /// Queue withdraw for the pool
    ///
    /// ### Arguments
    /// * `shares` - The amount of shares to queue for withdraw
    pub fn queue_for_withdraw(&mut self, shares: i128) {
        self.q4w += shares;
    }

    /// Dequeue queued for withdraw for the pool
    ///
    /// ### Arguments
    /// * `shares` - The amount of shares to dequeue from q4w
    pub fn dequeue_q4w(&mut self, e: &Env, shares: i128) {
        if shares > self.q4w {
            panic_with_error!(e, BackstopError::InsufficientFunds);
        }
        self.q4w -= shares;
    }
}

const BLND_WEIGHT: i128 = 8_000_000;
const PAIR_WEIGHT: i128 = 2_000_000;
const PAIR_VALUE_MULTIPLIER: i128 = 5;
const TOKEN_DECIMALS: u32 = 7;

#[derive(Clone, Debug, Eq, PartialEq)]
struct AssetValuation {
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
pub(crate) struct PoolValuation {
    pub active_values: ActivationValues,
    pub queued_values: ActivationValues,
    pub total_values: ActivationValues,
}

struct PoolTierValuation {
    active: AssetValuation,
    queued: AssetValuation,
    total: AssetValuation,
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
    AssetValuation { usdc_value: amount }
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
    use soroban_sdk::testutils::Address as _;

    use crate::testutils::{create_backstop, create_mock_pool_factory};

    use super::*;

    /********** require_is_from_pool_factory **********/

    #[test]
    fn test_require_is_from_pool_factory() {
        let e = Env::default();

        let backstop_address = create_backstop(&e);
        let pool_address = Address::generate(&e);

        let (_, mock_pool_factory) = create_mock_pool_factory(&e, &backstop_address);
        mock_pool_factory.set_mock_pool(&pool_address);

        e.as_contract(&backstop_address, || {
            require_is_from_pool_factory(&e, &pool_address, 0);
            assert!(true);
        });
    }

    #[test]
    fn test_require_is_from_pool_factory_skips_if_balance() {
        let e = Env::default();

        let backstop_address = create_backstop(&e);
        let pool_address = Address::generate(&e);

        // don't initialize factory to force failure if pool_address is checked

        e.as_contract(&backstop_address, || {
            require_is_from_pool_factory(&e, &pool_address, 1);
            assert!(true);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1004)")]
    fn test_require_is_from_pool_factory_not_valid() {
        let e = Env::default();

        let backstop_address = create_backstop(&e);
        let pool_address = Address::generate(&e);
        let not_pool_address = Address::generate(&e);

        let (_, mock_pool_factory) = create_mock_pool_factory(&e, &backstop_address);
        mock_pool_factory.set_mock_pool(&pool_address);

        e.as_contract(&backstop_address, || {
            require_is_from_pool_factory(&e, &not_pool_address, 0);
            assert!(false);
        });
    }

    /********** Logic **********/

    #[test]
    fn test_non_queued_tokens() {
        let pool_balance = PoolBalance {
            shares: 80321,
            tokens: 103302,
            q4w: 40001,
        };

        let non_queued_tokens = pool_balance.non_queued_tokens();
        assert_eq!(non_queued_tokens, 51857);
    }

    #[test]
    fn test_non_queued_tokens_no_shares() {
        let pool_balance = PoolBalance {
            shares: 0,
            tokens: 0,
            q4w: 0,
        };

        let non_queued_tokens = pool_balance.non_queued_tokens();
        assert_eq!(non_queued_tokens, 0);
    }

    #[test]
    fn test_non_queued_tokens_drained_backstop() {
        let pool_balance = PoolBalance {
            shares: 8765,
            tokens: 0,
            q4w: 4321,
        };

        let non_queued_tokens = pool_balance.non_queued_tokens();
        assert_eq!(non_queued_tokens, 0);
    }

    #[test]
    fn test_non_queued_tokens_full_q4w() {
        let pool_balance = PoolBalance {
            shares: 80321,
            tokens: 103302,
            q4w: 80321,
        };

        let non_queued_tokens = pool_balance.non_queued_tokens();
        assert_eq!(non_queued_tokens, 0);
    }

    #[test]
    fn test_convert_to_shares_no_shares() {
        let pool_balance = PoolBalance {
            shares: 0,
            tokens: 0,
            q4w: 0,
        };

        let to_convert = 1234567;
        let shares = pool_balance.convert_to_shares(to_convert);
        assert_eq!(shares, to_convert);
    }

    #[test]
    fn test_convert_to_shares_drained_backstop() {
        let pool_balance = PoolBalance {
            shares: 87654321,
            tokens: 0,
            q4w: 0,
        };

        let to_convert = 1234567;
        let shares = pool_balance.convert_to_shares(to_convert);
        assert_eq!(shares, 0);
    }

    #[test]
    fn test_convert_to_shares() {
        let pool_balance = PoolBalance {
            shares: 80321,
            tokens: 103302,
            q4w: 0,
        };

        let to_convert = 1234567;
        let shares = pool_balance.convert_to_shares(to_convert);
        assert_eq!(shares, 959920);
    }

    #[test]
    fn test_convert_to_tokens_no_shares() {
        let pool_balance = PoolBalance {
            shares: 0,
            tokens: 0,
            q4w: 0,
        };

        let to_convert = 1234567;
        let shares = pool_balance.convert_to_tokens(to_convert);
        assert_eq!(shares, 0);
    }

    #[test]
    fn test_convert_to_tokens_drained_backstop() {
        let pool_balance = PoolBalance {
            shares: 87654321,
            tokens: 0,
            q4w: 0,
        };

        let to_convert = 1234567;
        let shares = pool_balance.convert_to_tokens(to_convert);
        assert_eq!(shares, 0);
    }

    #[test]
    fn test_convert_to_tokens() {
        let pool_balance = PoolBalance {
            shares: 80321,
            tokens: 103302,
            q4w: 0,
        };

        let to_convert = 40000;
        let shares = pool_balance.convert_to_tokens(to_convert);
        assert_eq!(shares, 51444);
    }

    #[test]
    fn test_convert_to_tokens_all_shares() {
        let pool_balance = PoolBalance {
            shares: 80321,
            tokens: 103302,
            q4w: 0,
        };

        let to_convert = 80321;
        let shares = pool_balance.convert_to_tokens(to_convert);
        assert_eq!(shares, 103302);
    }

    #[test]
    fn test_deposit() {
        let mut pool_balance = PoolBalance {
            shares: 100,
            tokens: 200,
            q4w: 25,
        };

        pool_balance.deposit(50, 25);

        assert_eq!(pool_balance.shares, 125);
        assert_eq!(pool_balance.tokens, 250);
        assert_eq!(pool_balance.q4w, 25);
    }

    #[test]
    fn test_withdraw() {
        let e = Env::default();
        let mut pool_balance = PoolBalance {
            shares: 100,
            tokens: 200,
            q4w: 25,
        };

        pool_balance.withdraw(&e, 50, 25);

        assert_eq!(pool_balance.shares, 75);
        assert_eq!(pool_balance.tokens, 150);
        assert_eq!(pool_balance.q4w, 0);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1003)")]
    fn test_withdraw_too_much() {
        let e = Env::default();
        let mut pool_balance = PoolBalance {
            shares: 100,
            tokens: 200,
            q4w: 25,
        };

        pool_balance.withdraw(&e, 201, 25);
    }

    #[test]
    fn test_dequeue_q4w() {
        let e = Env::default();
        let mut pool_balance = PoolBalance {
            shares: 100,
            tokens: 200,
            q4w: 25,
        };

        pool_balance.dequeue_q4w(&e, 25);

        assert_eq!(pool_balance.shares, 100);
        assert_eq!(pool_balance.tokens, 200);
        assert_eq!(pool_balance.q4w, 0);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1003)")]
    fn test_dequeue_q4w_too_much() {
        let e = Env::default();
        let mut pool_balance = PoolBalance {
            shares: 100,
            tokens: 200,
            q4w: 25,
        };

        pool_balance.dequeue_q4w(&e, 26);
    }

    #[test]
    fn test_q4w() {
        let e = Env::default();
        let mut pool_balance = PoolBalance {
            shares: 100,
            tokens: 200,
            q4w: 25,
        };

        pool_balance.withdraw(&e, 50, 25);

        assert_eq!(pool_balance.shares, 75);
        assert_eq!(pool_balance.tokens, 150);
        assert_eq!(pool_balance.q4w, 0);
    }
}

#[cfg(test)]
mod tier_tests {
    use soroban_sdk::{
        testutils::{Address as _, Ledger, LedgerInfo},
        Address,
    };

    use crate::{
        constants::{MAX_Q4W_SIZE, Q4W_LOCK_TIME},
        testutils::{
            create_backstop, create_backstop_token, create_blnd_xlm_token,
            create_mock_pool_factory, create_usdc_token,
        },
        BackstopClient,
    };

    use super::*;

    #[test]
    fn three_tiers_keep_pool_and_user_accounting_isolated() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&e);
        let user = Address::generate(&e);
        let pool = Address::generate(&e);
        let backstop = create_backstop(&e);
        let (blnd_usdc, blnd_usdc_client) = create_backstop_token(&e, &backstop, &admin);
        let (blnd_xlm, blnd_xlm_client) = create_blnd_xlm_token(&e, &backstop, &admin);
        let (usdc, usdc_client) = create_usdc_token(&e, &backstop, &admin);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        factory.set_mock_pool(&pool);

        blnd_usdc_client.mint(&user, &100);
        blnd_xlm_client.mint(&user, &200);
        usdc_client.mint(&user, &300);

        let client = BackstopClient::new(&e, &backstop);
        assert_eq!(
            client.deposit(&crate::BackstopTier::BlndUsdc, &user, &pool, &100),
            100
        );
        assert_eq!(
            client.deposit(&crate::BackstopTier::BlndXlm, &user, &pool, &200),
            200
        );
        assert_eq!(
            client.deposit(&crate::BackstopTier::Usdc, &user, &pool, &300),
            300
        );

        assert_eq!(client.backstop_token(&BackstopTier::BlndUsdc), blnd_usdc);
        assert_eq!(client.backstop_token(&BackstopTier::BlndXlm), blnd_xlm);
        assert_eq!(client.backstop_token(&BackstopTier::Usdc), usdc);
        let pool_data = client.pool_data(&pool);
        assert_eq!(pool_data.blnd_usdc.tokens, 100);
        assert_eq!(pool_data.blnd_usdc.shares, 100);
        assert_eq!(pool_data.blnd_xlm.tokens, 200);
        assert_eq!(pool_data.blnd_xlm.shares, 200);
        assert_eq!(pool_data.usdc.tokens, 300);
        assert_eq!(pool_data.usdc.shares, 300);
        let user_balance = client.user_balance(&BackstopTier::Usdc, &pool, &user);
        assert_eq!(user_balance.shares, 300);
        assert!(user_balance.q4w.is_empty());
    }

    #[test]
    fn q4w_limit_is_aggregate_and_withdrawal_is_tier_specific() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 1,
            timestamp: 10_000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3_110_400,
        });

        let admin = Address::generate(&e);
        let user = Address::generate(&e);
        let recipient = Address::generate(&e);
        let pool = Address::generate(&e);
        let backstop = create_backstop(&e);
        let (_, blnd_usdc_client) = create_backstop_token(&e, &backstop, &admin);
        let (_, blnd_xlm_client) = create_blnd_xlm_token(&e, &backstop, &admin);
        let (_, usdc_client) = create_usdc_token(&e, &backstop, &admin);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        factory.set_mock_pool(&pool);

        blnd_usdc_client.mint(&user, &100);
        blnd_xlm_client.mint(&user, &100);
        usdc_client.mint(&user, &100);
        let client = BackstopClient::new(&e, &backstop);
        client.deposit(&crate::BackstopTier::BlndUsdc, &user, &pool, &100);
        client.deposit(&crate::BackstopTier::BlndXlm, &user, &pool, &100);
        client.deposit(&crate::BackstopTier::Usdc, &user, &pool, &100);

        for _ in 0..10 {
            client.queue_withdrawal(&crate::BackstopTier::BlndUsdc, &user, &pool, &1);
        }
        for _ in 0..5 {
            client.queue_withdrawal(&crate::BackstopTier::BlndXlm, &user, &pool, &1);
            client.queue_withdrawal(&crate::BackstopTier::Usdc, &user, &pool, &1);
        }
        assert_eq!(
            client
                .user_balance(&BackstopTier::BlndUsdc, &pool, &user)
                .q4w
                .len()
                + client
                    .user_balance(&BackstopTier::BlndXlm, &pool, &user)
                    .q4w
                    .len()
                + client
                    .user_balance(&BackstopTier::Usdc, &pool, &user)
                    .q4w
                    .len(),
            MAX_Q4W_SIZE
        );
        assert!(client
            .try_queue_withdrawal(&crate::BackstopTier::Usdc, &user, &pool, &1)
            .is_err());

        client.dequeue_withdrawal(&crate::BackstopTier::BlndUsdc, &user, &pool, &1);
        client.queue_withdrawal(&crate::BackstopTier::BlndXlm, &user, &pool, &1);
        e.ledger().set_timestamp(10_000 + Q4W_LOCK_TIME + 1);
        assert_eq!(
            client.withdraw(&crate::BackstopTier::BlndXlm, &user, &pool, &6, &recipient),
            6
        );
        assert_eq!(blnd_xlm_client.balance(&recipient), 6);
        let user_balance = client.user_balance(&BackstopTier::BlndXlm, &pool, &user);
        assert_eq!(user_balance.shares, 94);
        assert!(user_balance.q4w.is_empty());
    }

    #[test]
    fn deposit_requires_factory_registration_only() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&e);
        let user = Address::generate(&e);
        let pool = Address::generate(&e);
        let backstop = create_backstop(&e);
        let (_, usdc_client) = create_usdc_token(&e, &backstop, &admin);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        let client = BackstopClient::new(&e, &backstop);

        usdc_client.mint(&user, &200);
        assert!(client
            .try_deposit(&crate::BackstopTier::Usdc, &user, &pool, &100)
            .is_err());
        assert_eq!(usdc_client.balance(&user), 200);

        factory.set_pool(&pool);
        assert_eq!(
            client.deposit(&crate::BackstopTier::Usdc, &user, &pool, &100),
            100
        );
        assert_eq!(usdc_client.balance(&user), 100);
    }
}

#[cfg(test)]
mod valuation_tests {
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
                usdc_value: 25 * SCALAR_7,
            }
        );
        assert_eq!(
            blnd_xlm,
            AssetValuation {
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
