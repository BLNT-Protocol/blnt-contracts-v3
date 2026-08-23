use sep_41_token::{StellarAssetClient, TokenClient};
use soroban_fixed_point_math::FixedPoint;
use soroban_sdk::{contracttype, panic_with_error, unwrap::UnwrapOptimized, Address, Env, I256};

use crate::{
    constants::{ACTIVATION_THRESHOLD_USDC, SCALAR_7},
    dependencies::{BackstopTierConfig, CometClient, FactoryBackstopAsset, PoolFactoryClient},
    errors::BackstopError,
    storage,
};

const MAX_TAKE_RATE_WEIGHT: u32 = 10;

/// One pool's complete configured backstop accounting and valuation.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct PoolBackstopData {
    /// Aggregate transferable USDC value excluding queued withdrawals.
    pub active_value: i128,
    /// Queued value divided by total active-plus-queued value, rounded up.
    pub q4w_pct: i128,
    /// Tier data in configured loss-waterfall order.
    pub tiers: soroban_sdk::Vec<PoolTierData>,
}

/// One configured tier's compact accounting and verified valuation.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct PoolTierData {
    pub asset: BackstopAsset,
    pub blnd_emission_eligible: bool,
    pub take_rate_weight: u32,
    pub token: Address,
    pub tokens: i128,
    pub shares: i128,
    /// Transferable USDC-equivalent value; zero while plain USDC is deauthorized.
    pub value: i128,
}

/// The canonical asset assigned to one configured loss tier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[contracttype]
pub enum BackstopAsset {
    BlndXlm,
    BlndUsdc,
    Usdc,
    Xlm,
}

/// A backstop asset's immutable position in one pool's loss waterfall.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[contracttype]
pub enum BackstopTier {
    FirstLoss,
    SecondLoss,
    ThirdLoss,
}

pub fn tier_index(tier: BackstopTier) -> u32 {
    match tier {
        BackstopTier::FirstLoss => 0,
        BackstopTier::SecondLoss => 1,
        BackstopTier::ThirdLoss => 2,
    }
}

pub fn tier_from_index(e: &Env, index: u32) -> BackstopTier {
    match index {
        0 => BackstopTier::FirstLoss,
        1 => BackstopTier::SecondLoss,
        2 => BackstopTier::ThirdLoss,
        _ => panic_with_error!(e, BackstopError::BadRequest),
    }
}

pub fn pool_backstop_config(e: &Env, pool: &Address) -> soroban_sdk::Vec<BackstopTierConfig> {
    if let Some(config) = storage::get_pool_backstop_config(e, pool) {
        return config;
    }
    let factory = PoolFactoryClient::new(e, &storage::get_pool_factory(e));
    if !factory.is_pool(pool) {
        panic_with_error!(e, BackstopError::NotPool);
    }
    let config = factory.backstop_config(pool);
    validate_pool_backstop_config(e, &config);
    storage::set_pool_backstop_config(e, pool, &config);
    config
}

pub fn tier_config(e: &Env, pool: &Address, tier: BackstopTier) -> BackstopTierConfig {
    pool_backstop_config(e, pool)
        .get(tier_index(tier))
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::BadRequest))
}

pub fn tier_token(e: &Env, pool: &Address, tier: BackstopTier) -> Address {
    asset_token(e, tier_asset(e, pool, tier))
}

pub fn tier_asset(e: &Env, pool: &Address, tier: BackstopTier) -> BackstopAsset {
    from_factory_asset(tier_config(e, pool, tier).asset)
}

pub fn tier_for_token(e: &Env, pool: &Address, token: &Address) -> Option<BackstopTier> {
    let config = pool_backstop_config(e, pool);
    for (index, tier) in config.iter().enumerate() {
        if asset_token(e, from_factory_asset(tier.asset)) == *token {
            return Some(tier_from_index(e, index as u32));
        }
    }
    None
}

pub fn is_blnd_emission_tier(e: &Env, pool: &Address, tier: BackstopTier) -> bool {
    matches!(
        tier_asset(e, pool, tier),
        BackstopAsset::BlndUsdc | BackstopAsset::BlndXlm
    )
}

pub fn asset_token(e: &Env, asset: BackstopAsset) -> Address {
    match asset {
        BackstopAsset::BlndXlm => storage::get_blnd_xlm_token(e),
        BackstopAsset::BlndUsdc => storage::get_blnd_usdc_token(e),
        BackstopAsset::Usdc => storage::get_usdc_token(e),
        BackstopAsset::Xlm => storage::get_xlm_token(e),
    }
}

fn from_factory_asset(asset: FactoryBackstopAsset) -> BackstopAsset {
    match asset {
        FactoryBackstopAsset::BlndXlm => BackstopAsset::BlndXlm,
        FactoryBackstopAsset::BlndUsdc => BackstopAsset::BlndUsdc,
        FactoryBackstopAsset::Usdc => BackstopAsset::Usdc,
        FactoryBackstopAsset::Xlm => BackstopAsset::Xlm,
    }
}

pub(crate) fn load_pool_backstop_data(e: &Env, pool: &Address) -> PoolBackstopData {
    let valuation = build_pool_valuation(e, pool);
    let config = pool_backstop_config(e, pool);
    let mut tiers = soroban_sdk::Vec::new(e);
    for (index, tier_config) in config.iter().enumerate() {
        let tier = tier_from_index(e, index as u32);
        let balance = storage::get_pool_balance_for_tier(e, tier, pool);
        tiers.push_back(tier_data(
            e,
            &tier_config,
            &balance,
            valuation.total_values.tiers.get(index as u32).unwrap(),
        ));
    }
    PoolBackstopData {
        active_value: sum_activation_values(e, &valuation.active_values),
        q4w_pct: calculate_q4w_percentage(e, &valuation.active_values, &valuation.queued_values),
        tiers,
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
        pool_backstop_config(e, address);
    }
}

/// Verify a pool is registered by the configured pool factory.
pub fn require_registered_pool(e: &Env, address: &Address) {
    pool_backstop_config(e, address);
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
    pub tiers: soroban_sdk::Vec<i128>,
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

fn tier_data(
    e: &Env,
    config: &BackstopTierConfig,
    balance: &PoolBalance,
    value: i128,
) -> PoolTierData {
    let asset = from_factory_asset(config.asset);
    PoolTierData {
        asset,
        blnd_emission_eligible: matches!(asset, BackstopAsset::BlndUsdc | BackstopAsset::BlndXlm),
        take_rate_weight: config.take_rate_weight,
        token: asset_token(e, asset),
        tokens: balance.tokens,
        shares: balance.shares,
        value,
    }
}

pub(crate) fn build_pool_valuation(e: &Env, pool: &Address) -> PoolValuation {
    let config = pool_backstop_config(e, pool);
    let mut amounts = soroban_sdk::Vec::new(e);
    let mut any_tokens = false;
    let mut needs_anchor = false;
    let mut needs_target = false;
    for index in 0..config.len() {
        let partition = pool_tier_asset_partition(e, tier_from_index(e, index), pool);
        any_tokens |= partition.2 > 0;
        if partition.2 > 0 {
            match from_factory_asset(config.get(index).unwrap().asset) {
                BackstopAsset::BlndUsdc => needs_anchor = true,
                BackstopAsset::BlndXlm | BackstopAsset::Xlm => {
                    needs_anchor = true;
                    needs_target = true;
                }
                BackstopAsset::Usdc => {}
            }
        }
        amounts.push_back(partition);
    }

    #[cfg(any(test, feature = "testutils"))]
    if let Some(should_fail) = test_valuation_override(e) {
        if should_fail {
            panic_with_error!(e, BackstopError::InvalidValuation);
        }
        let mut active = soroban_sdk::Vec::new(e);
        let mut queued = soroban_sdk::Vec::new(e);
        let mut total = soroban_sdk::Vec::new(e);
        for (index, partition) in amounts.iter().enumerate() {
            let config = config.get(index as u32).unwrap();
            if configured_tier_is_transferable(e, &config) {
                active.push_back(partition.0);
                queued.push_back(partition.1);
                total.push_back(partition.2);
            } else {
                active.push_back(0);
                queued.push_back(0);
                total.push_back(0);
            }
        }
        return PoolValuation {
            active_values: ActivationValues { tiers: active },
            queued_values: ActivationValues { tiers: queued },
            total_values: ActivationValues { tiers: total },
        };
    }

    if !any_tokens {
        let mut zeroes = soroban_sdk::Vec::new(e);
        for _ in 0..config.len() {
            zeroes.push_back(0);
        }
        return PoolValuation {
            active_values: ActivationValues {
                tiers: zeroes.clone(),
            },
            queued_values: ActivationValues {
                tiers: zeroes.clone(),
            },
            total_values: ActivationValues { tiers: zeroes },
        };
    }

    let blnd = storage::get_blnd_token(e);
    let anchor = needs_anchor.then(|| {
        read_comet(
            e,
            &storage::get_blnd_usdc_token(e),
            &blnd,
            &storage::get_usdc_token(e),
        )
    });
    let anchor_value = anchor
        .as_ref()
        .map(|composition| checked_mul(e, composition.pair_reserve, PAIR_VALUE_MULTIPLIER));
    let target = needs_target.then(|| {
        read_comet(
            e,
            &storage::get_blnd_xlm_token(e),
            &blnd,
            &storage::get_xlm_token(e),
        )
    });
    let mut active = soroban_sdk::Vec::new(e);
    let mut queued = soroban_sdk::Vec::new(e);
    let mut total = soroban_sdk::Vec::new(e);
    for index in 0..config.len() {
        let tier_config = config.get(index).unwrap();
        let quotes = quote_configured_tier(
            e,
            &tier_config,
            amounts.get(index).unwrap(),
            anchor.as_ref(),
            anchor_value,
            target.as_ref(),
        );
        if configured_tier_is_transferable(e, &tier_config) {
            active.push_back(quotes.active.usdc_value);
            queued.push_back(quotes.queued.usdc_value);
            total.push_back(quotes.total.usdc_value);
        } else {
            active.push_back(0);
            queued.push_back(0);
            total.push_back(0);
        }
    }
    PoolValuation {
        active_values: ActivationValues { tiers: active },
        queued_values: ActivationValues { tiers: queued },
        total_values: ActivationValues { tiers: total },
    }
}

fn quote_configured_tier(
    e: &Env,
    config: &BackstopTierConfig,
    amounts: (i128, i128, i128),
    anchor: Option<&CometComposition>,
    anchor_value: Option<i128>,
    target: Option<&CometComposition>,
) -> PoolTierValuation {
    for amount in [amounts.0, amounts.1, amounts.2] {
        if amount < 0 {
            panic_with_error!(e, BackstopError::InvalidValuation);
        }
    }
    if amounts.2 == 0 {
        return unit_pool_tier_valuation(amounts);
    }
    match from_factory_asset(config.asset) {
        BackstopAsset::BlndUsdc => pool_tier_valuation(
            e,
            amounts,
            anchor_value.unwrap_or_else(|| panic_with_error!(e, BackstopError::InvalidValuation)),
            anchor.unwrap_or_else(|| panic_with_error!(e, BackstopError::InvalidValuation)),
        ),
        BackstopAsset::BlndXlm => {
            let anchor =
                anchor.unwrap_or_else(|| panic_with_error!(e, BackstopError::InvalidValuation));
            let anchor_value = anchor_value
                .unwrap_or_else(|| panic_with_error!(e, BackstopError::InvalidValuation));
            let target =
                target.unwrap_or_else(|| panic_with_error!(e, BackstopError::InvalidValuation));
            let target_value =
                mul_div_floor(e, target.blnd_reserve, anchor_value, anchor.blnd_reserve);
            pool_tier_valuation(e, amounts, target_value, target)
        }
        BackstopAsset::Usdc => unit_pool_tier_valuation(amounts),
        BackstopAsset::Xlm => xlm_pool_tier_valuation(
            e,
            amounts,
            anchor.unwrap_or_else(|| panic_with_error!(e, BackstopError::InvalidValuation)),
            target.unwrap_or_else(|| panic_with_error!(e, BackstopError::InvalidValuation)),
        ),
    }
}

fn configured_tier_is_transferable(e: &Env, config: &BackstopTierConfig) -> bool {
    if from_factory_asset(config.asset) != BackstopAsset::Usdc {
        return true;
    }
    let usdc = storage::get_usdc_token(e);
    StellarAssetClient::new(e, &usdc).authorized(&e.current_contract_address())
}

fn xlm_pool_tier_valuation(
    e: &Env,
    amounts: (i128, i128, i128),
    anchor: &CometComposition,
    target: &CometComposition,
) -> PoolTierValuation {
    PoolTierValuation {
        active: xlm_asset_valuation(e, amounts.0, anchor, target),
        queued: xlm_asset_valuation(e, amounts.1, anchor, target),
        total: xlm_asset_valuation(e, amounts.2, anchor, target),
    }
}

fn xlm_asset_valuation(
    e: &Env,
    amount: i128,
    anchor: &CometComposition,
    target: &CometComposition,
) -> AssetValuation {
    if amount < 0 {
        panic_with_error!(e, BackstopError::InvalidValuation);
    }
    let numerator = I256::from_i128(e, amount)
        .mul(&I256::from_i128(e, anchor.pair_reserve))
        .mul(&I256::from_i128(e, target.blnd_reserve));
    let denominator =
        I256::from_i128(e, anchor.blnd_reserve).mul(&I256::from_i128(e, target.pair_reserve));
    AssetValuation {
        usdc_value: numerator
            .div(&denominator)
            .to_i128()
            .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError)),
    }
}

#[cfg(test)]
fn quote_pool_lp_amounts(
    e: &Env,
    blnd_usdc: (i128, i128, i128),
    blnd_xlm: (i128, i128, i128),
) -> (PoolTierValuation, PoolTierValuation) {
    let blnd = storage::get_blnd_token(e);
    let usdc = storage::get_usdc_token(e);
    let anchor = read_comet(e, &storage::get_blnd_usdc_token(e), &blnd, &usdc);
    let anchor_value = checked_mul(e, anchor.pair_reserve, PAIR_VALUE_MULTIPLIER);
    let blnd_usdc_quote = pool_tier_valuation(e, blnd_usdc, anchor_value, &anchor);
    let target = read_comet(
        e,
        &storage::get_blnd_xlm_token(e),
        &blnd,
        &storage::get_xlm_token(e),
    );
    let target_value = mul_div_floor(e, target.blnd_reserve, anchor_value, anchor.blnd_reserve);
    (
        blnd_usdc_quote,
        pool_tier_valuation(e, blnd_xlm, target_value, &target),
    )
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

pub fn quote_activation(e: &Env, values: &ActivationValues) -> ActivationQuote {
    let eligible_value = sum_activation_values(e, values);
    ActivationQuote {
        eligible_value,
        meets_threshold: eligible_value >= ACTIVATION_THRESHOLD_USDC,
        required_value: ACTIVATION_THRESHOLD_USDC,
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

fn validate_pool_backstop_config(e: &Env, config: &soroban_sdk::Vec<BackstopTierConfig>) {
    if config.is_empty() || config.len() > 3 {
        panic_with_error!(e, BackstopError::InvalidBackstopValuation);
    }
    let mut previous_weight = None;
    for (index, tier) in config.iter().enumerate() {
        if tier.take_rate_weight == 0 || tier.take_rate_weight > MAX_TAKE_RATE_WEIGHT {
            panic_with_error!(e, BackstopError::InvalidBackstopValuation);
        }
        if let Some(weight) = previous_weight {
            if weight <= tier.take_rate_weight {
                panic_with_error!(e, BackstopError::InvalidBackstopValuation);
            }
        }
        previous_weight = Some(tier.take_rate_weight);
        let asset = from_factory_asset(tier.asset);
        if TokenClient::new(e, &asset_token(e, asset)).decimals() != TOKEN_DECIMALS {
            panic_with_error!(e, BackstopError::InvalidBackstopValuation);
        }
        for later in config.iter().skip(index + 1) {
            if tier.asset == later.asset {
                panic_with_error!(e, BackstopError::AssetConfigurationCollision);
            }
        }
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
    let mut total = 0_i128;
    for value in values.tiers.iter() {
        if value < 0 {
            panic_with_error!(e, BackstopError::InvalidActivationValue);
        }
        total = total
            .checked_add(value)
            .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    }
    total
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
        mock_pool_factory.set_pool(&pool_address);

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
        mock_pool_factory.set_pool(&pool_address);

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
        vec, Address,
    };

    use crate::{
        constants::{MAX_Q4W_SIZE, Q4W_LOCK_TIME},
        testutils::{
            create_backstop, create_backstop_token, create_blnd_xlm_token, create_mock_pool,
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
        factory.set_pool(&pool);

        blnd_usdc_client.mint(&user, &100);
        blnd_xlm_client.mint(&user, &200);
        usdc_client.mint(&user, &300);

        let client = BackstopClient::new(&e, &backstop);
        assert_eq!(
            client.deposit(&crate::BackstopTier::SecondLoss, &user, &pool, &100),
            100
        );
        assert_eq!(
            client.deposit(&crate::BackstopTier::FirstLoss, &user, &pool, &200),
            200
        );
        assert_eq!(
            client.deposit(&crate::BackstopTier::ThirdLoss, &user, &pool, &300),
            300
        );

        assert_eq!(
            client.backstop_token(&BackstopTier::SecondLoss, &pool),
            blnd_usdc
        );
        assert_eq!(
            client.backstop_token(&BackstopTier::FirstLoss, &pool),
            blnd_xlm
        );
        assert_eq!(client.backstop_token(&BackstopTier::ThirdLoss, &pool), usdc);
        let pool_data = client.pool_data(&pool);
        assert_eq!(pool_data.tiers.get(1).unwrap().tokens, 100);
        assert_eq!(pool_data.tiers.get(1).unwrap().shares, 100);
        assert_eq!(pool_data.tiers.get(0).unwrap().tokens, 200);
        assert_eq!(pool_data.tiers.get(0).unwrap().shares, 200);
        assert_eq!(pool_data.tiers.get(2).unwrap().tokens, 300);
        assert_eq!(pool_data.tiers.get(2).unwrap().shares, 300);
        let user_balance = client.user_balance(&BackstopTier::ThirdLoss, &pool, &user);
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
        let backstop = create_backstop(&e);
        let (pool, _) = create_mock_pool(&e, &backstop);
        let (_, blnd_usdc_client) = create_backstop_token(&e, &backstop, &admin);
        let (_, blnd_xlm_client) = create_blnd_xlm_token(&e, &backstop, &admin);
        let (_, usdc_client) = create_usdc_token(&e, &backstop, &admin);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        factory.set_pool(&pool);

        blnd_usdc_client.mint(&user, &100);
        blnd_xlm_client.mint(&user, &100);
        usdc_client.mint(&user, &100);
        let client = BackstopClient::new(&e, &backstop);
        client.deposit(&crate::BackstopTier::SecondLoss, &user, &pool, &100);
        client.deposit(&crate::BackstopTier::FirstLoss, &user, &pool, &100);
        client.deposit(&crate::BackstopTier::ThirdLoss, &user, &pool, &100);

        for _ in 0..10 {
            client.queue_withdrawal(&crate::BackstopTier::SecondLoss, &user, &pool, &1);
        }
        for _ in 0..5 {
            client.queue_withdrawal(&crate::BackstopTier::FirstLoss, &user, &pool, &1);
            client.queue_withdrawal(&crate::BackstopTier::ThirdLoss, &user, &pool, &1);
        }
        assert_eq!(
            client
                .user_balance(&BackstopTier::SecondLoss, &pool, &user)
                .q4w
                .len()
                + client
                    .user_balance(&BackstopTier::FirstLoss, &pool, &user)
                    .q4w
                    .len()
                + client
                    .user_balance(&BackstopTier::ThirdLoss, &pool, &user)
                    .q4w
                    .len(),
            MAX_Q4W_SIZE
        );
        assert!(client
            .try_queue_withdrawal(&crate::BackstopTier::ThirdLoss, &user, &pool, &1)
            .is_err());

        client.dequeue_withdrawal(&crate::BackstopTier::SecondLoss, &user, &pool, &1);
        client.queue_withdrawal(&crate::BackstopTier::FirstLoss, &user, &pool, &1);
        e.ledger().set_timestamp(10_000 + Q4W_LOCK_TIME + 1);
        assert_eq!(
            client.withdraw(
                &crate::BackstopTier::FirstLoss,
                &user,
                &pool,
                &6,
                &recipient
            ),
            6
        );
        assert_eq!(blnd_xlm_client.balance(&recipient), 6);
        let user_balance = client.user_balance(&BackstopTier::FirstLoss, &pool, &user);
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
            .try_deposit(&crate::BackstopTier::ThirdLoss, &user, &pool, &100)
            .is_err());
        assert_eq!(usdc_client.balance(&user), 200);

        factory.set_pool(&pool);
        assert_eq!(
            client.deposit(&crate::BackstopTier::ThirdLoss, &user, &pool, &100),
            100
        );
        assert_eq!(usdc_client.balance(&user), 100);
    }

    #[test]
    fn omitted_trailing_tiers_are_rejected_by_public_getters() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        let backstop = create_backstop(&e);
        let pool = Address::generate(&e);
        let user = Address::generate(&e);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        factory.set_pool_config(
            &pool,
            &vec![
                &e,
                mock_pool_factory::BackstopTierConfig {
                    asset: mock_pool_factory::BackstopAsset::Usdc,
                    take_rate_weight: 1,
                },
            ],
        );

        let client = BackstopClient::new(&e, &backstop);
        assert!(client
            .try_user_balance(&BackstopTier::SecondLoss, &pool, &user)
            .is_err());
        assert!(client
            .try_backstop_token(&BackstopTier::ThirdLoss, &pool)
            .is_err());
    }
}

#[cfg(test)]
mod valuation_tests {
    use mock_pool_factory::{BackstopAsset as FactoryBackstopAsset, BackstopTierConfig};
    use sep_41_token::{testutils::MockTokenClient, StellarAssetClient};
    use soroban_sdk::{testutils::Address as _, vec, Address};

    use crate::{
        storage,
        testutils::{
            create_backstop, create_backstop_token, create_backstop_with_real_comets,
            create_blnd_xlm_token, create_mock_pool_factory, create_usdc_token,
        },
        BackstopClient,
    };

    use super::*;

    #[test]
    fn rejects_out_of_range_or_non_descending_tier_weights() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        let backstop = create_backstop(&e);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        let client = BackstopClient::new(&e, &backstop);

        let excessive = Address::generate(&e);
        factory.set_pool_config(
            &excessive,
            &vec![
                &e,
                BackstopTierConfig {
                    asset: FactoryBackstopAsset::Usdc,
                    take_rate_weight: 11,
                },
            ],
        );
        assert!(client.try_pool_data(&excessive).is_err());

        let equal = Address::generate(&e);
        factory.set_pool_config(
            &equal,
            &vec![
                &e,
                BackstopTierConfig {
                    asset: FactoryBackstopAsset::BlndXlm,
                    take_rate_weight: 4,
                },
                BackstopTierConfig {
                    asset: FactoryBackstopAsset::Usdc,
                    take_rate_weight: 4,
                },
            ],
        );
        assert!(client.try_pool_data(&equal).is_err());

        let ascending = Address::generate(&e);
        factory.set_pool_config(
            &ascending,
            &vec![
                &e,
                BackstopTierConfig {
                    asset: FactoryBackstopAsset::BlndXlm,
                    take_rate_weight: 3,
                },
                BackstopTierConfig {
                    asset: FactoryBackstopAsset::Usdc,
                    take_rate_weight: 4,
                },
            ],
        );
        assert!(client.try_pool_data(&ascending).is_err());
    }

    fn values(e: &Env, blnd_usdc: i128, blnd_xlm: i128, usdc: i128) -> ActivationValues {
        ActivationValues {
            tiers: soroban_sdk::vec![e, blnd_xlm, blnd_usdc, usdc],
        }
    }

    #[test]
    fn activation_values_all_tiers_equally_and_uses_one_threshold() {
        let e = Env::default();
        let threshold = values(&e, 4_000 * SCALAR_7, 3_500 * SCALAR_7, 5_000 * SCALAR_7);
        assert_eq!(
            quote_activation(&e, &threshold),
            ActivationQuote {
                eligible_value: ACTIVATION_THRESHOLD_USDC,
                meets_threshold: true,
                required_value: ACTIVATION_THRESHOLD_USDC,
            }
        );

        let below_threshold = values(&e, 0, 0, ACTIVATION_THRESHOLD_USDC - 1);
        assert!(!quote_activation(&e, &below_threshold).meets_threshold);
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
        let active = values(&e, 4_000 * SCALAR_7, 3_000 * SCALAR_7, 0);
        let queued = values(&e, 0, 0, 3_000 * SCALAR_7);
        assert_eq!(calculate_q4w_percentage(&e, &active, &queued), 3_000_000);

        let rounded = calculate_q4w_percentage(
            &e,
            &values(&e, 0, 0, 2 * SCALAR_7),
            &values(&e, 0, 0, SCALAR_7),
        );
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
        factory.set_pool(&pool);

        blnd_usdc.mint(&user, &(4_000 * SCALAR_7));
        blnd_xlm.mint(&user, &(5_000 * SCALAR_7));
        usdc.mint(&user, &(6_500 * SCALAR_7));
        let backstop_client = BackstopClient::new(&e, &backstop);
        backstop_client.deposit(
            &crate::BackstopTier::SecondLoss,
            &user,
            &pool,
            &(4_000 * SCALAR_7),
        );
        backstop_client.deposit(
            &crate::BackstopTier::FirstLoss,
            &user,
            &pool,
            &(5_000 * SCALAR_7),
        );
        backstop_client.deposit(
            &crate::BackstopTier::ThirdLoss,
            &user,
            &pool,
            &(6_500 * SCALAR_7),
        );
        backstop_client.queue_withdrawal(
            &crate::BackstopTier::SecondLoss,
            &user,
            &pool,
            &(1_000 * SCALAR_7),
        );
        backstop_client.queue_withdrawal(
            &crate::BackstopTier::FirstLoss,
            &user,
            &pool,
            &(2_000 * SCALAR_7),
        );

        let pool_data = backstop_client.pool_data(&pool);
        let blnd_xlm_data = pool_data.tiers.get(0).unwrap();
        assert_eq!(blnd_xlm_data.token, blnd_xlm.address);
        assert_eq!(blnd_xlm_data.tokens, 5_000 * SCALAR_7);
        assert_eq!(blnd_xlm_data.shares, 5_000 * SCALAR_7);
        assert_eq!(blnd_xlm_data.value, 5_000 * SCALAR_7);
        assert_eq!(blnd_xlm_data.take_rate_weight, 4);
        let blnd_usdc_data = pool_data.tiers.get(1).unwrap();
        assert_eq!(blnd_usdc_data.token, blnd_usdc.address);
        assert_eq!(blnd_usdc_data.tokens, 4_000 * SCALAR_7);
        assert_eq!(blnd_usdc_data.shares, 4_000 * SCALAR_7);
        assert_eq!(blnd_usdc_data.value, 4_000 * SCALAR_7);
        assert_eq!(blnd_usdc_data.take_rate_weight, 3);
        let usdc_data = pool_data.tiers.get(2).unwrap();
        assert_eq!(usdc_data.token, usdc.address);
        assert_eq!(usdc_data.tokens, 6_500 * SCALAR_7);
        assert_eq!(usdc_data.shares, 6_500 * SCALAR_7);
        assert_eq!(usdc_data.value, 6_500 * SCALAR_7);
        assert_eq!(usdc_data.take_rate_weight, 2);
        assert_eq!(pool_data.active_value, ACTIVATION_THRESHOLD_USDC);
        assert_eq!(pool_data.q4w_pct, 1_935_484);
        let quote = e.as_contract(&backstop, || {
            let valuation = build_pool_valuation(&e, &pool);
            quote_activation(&e, &valuation.active_values)
        });
        assert_eq!(quote.eligible_value, ACTIVATION_THRESHOLD_USDC);
        assert!(quote.meets_threshold);
    }

    #[test]
    fn deauthorized_usdc_has_zero_value_until_reauthorized() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&e);
        let user = Address::generate(&e);
        let pool = Address::generate(&e);
        let backstop = create_backstop(&e);
        let (usdc, usdc_client) = create_usdc_token(&e, &backstop, &admin);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        factory.set_pool_config(
            &pool,
            &vec![
                &e,
                BackstopTierConfig {
                    asset: FactoryBackstopAsset::Usdc,
                    take_rate_weight: 1,
                },
            ],
        );
        usdc_client.mint(&user, &ACTIVATION_THRESHOLD_USDC);
        let client = BackstopClient::new(&e, &backstop);
        client.deposit(
            &BackstopTier::FirstLoss,
            &user,
            &pool,
            &ACTIVATION_THRESHOLD_USDC,
        );

        assert_eq!(
            client.pool_data(&pool).active_value,
            ACTIVATION_THRESHOLD_USDC
        );
        StellarAssetClient::new(&e, &usdc).set_authorized(&backstop, &false);

        let deauthorized = client.pool_data(&pool);
        assert_eq!(deauthorized.active_value, 0);
        assert_eq!(deauthorized.q4w_pct, 0);
        assert_eq!(
            deauthorized.tiers.first().unwrap().tokens,
            ACTIVATION_THRESHOLD_USDC
        );
        assert_eq!(
            deauthorized.tiers.first().unwrap().shares,
            ACTIVATION_THRESHOLD_USDC
        );
        assert_eq!(deauthorized.tiers.first().unwrap().value, 0);

        StellarAssetClient::new(&e, &usdc).set_authorized(&backstop, &true);
        assert_eq!(
            client.pool_data(&pool).active_value,
            ACTIVATION_THRESHOLD_USDC
        );
    }

    #[test]
    fn xlm_tier_uses_the_canonical_comet_cross_price() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited();

        let user = Address::generate(&e);
        let pool = Address::generate(&e);
        let backstop = create_backstop_with_real_comets(&e);
        let xlm = e.as_contract(&backstop, || storage::get_xlm_token(&e));
        let xlm_client = MockTokenClient::new(&e, &xlm);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        factory.set_pool_config(
            &pool,
            &vec![
                &e,
                BackstopTierConfig {
                    asset: FactoryBackstopAsset::Xlm,
                    take_rate_weight: 10,
                },
            ],
        );
        e.as_contract(&backstop, || set_test_valuation_override(&e, None));

        let amount = ACTIVATION_THRESHOLD_USDC;
        xlm_client.mint(&user, &amount);
        let client = BackstopClient::new(&e, &backstop);
        client.deposit(&BackstopTier::FirstLoss, &user, &pool, &amount);

        let data = client.pool_data(&pool);
        assert_eq!(data.tiers.len(), 1);
        let tier = data.tiers.first().unwrap();
        assert_eq!(tier.asset, BackstopAsset::Xlm);
        assert_eq!(tier.token, xlm);
        assert_eq!(tier.take_rate_weight, 10);
        assert!(!tier.blnd_emission_eligible);
        assert_eq!(tier.value, ACTIVATION_THRESHOLD_USDC);
        assert_eq!(data.active_value, ACTIVATION_THRESHOLD_USDC);
    }
}
