#![no_std]

use soroban_sdk::{
    contract, contractclient, contracterror, contractimpl, contracttype, panic_with_error, Address,
    Env, Map, Symbol, Vec, I256,
};

const ADAPTER_VERSION: u32 = 1;
const SCALAR_7: i128 = 10_000_000;
const BLND_WEIGHT: i128 = 8_000_000;
const PAIR_WEIGHT: i128 = 2_000_000;
const TOKEN_DECIMALS: u32 = 7;
const MIN_TWAP_RECORDS: u32 = 2;
const MAX_TWAP_RECORDS: u32 = 25;
const MIN_TWAP_WINDOW_SECONDS: u64 = 30 * 60;
const MAX_TWAP_WINDOW_SECONDS: u64 = 24 * 60 * 60;
const MAX_PRICE_AGE_SECONDS: u64 = 60 * 60;
const DAY_IN_LEDGERS: u32 = 17_280;
const INSTANCE_TTL_THRESHOLD: u32 = 89 * DAY_IN_LEDGERS;
const INSTANCE_TTL_BUMP: u32 = 90 * DAY_IN_LEDGERS;

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct AssetValuation {
    pub underlying_blnd: i128,
    pub usdc_value: i128,
    pub valid_until: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct AdapterBinding {
    pub blnd: Address,
    pub blnd_usdc: Address,
    pub blnd_xlm: Address,
    pub usdc: Address,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct PriceData {
    pub price: i128,
    pub timestamp: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub enum OracleAsset {
    Stellar(Address),
    Other(Symbol),
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct AdapterConfig {
    pub blnd: Address,
    pub blnd_usdc: Address,
    pub blnd_xlm: Address,
    pub max_price_age: u64,
    pub oracle: Address,
    pub oracle_base: OracleAsset,
    pub oracle_decimals: u32,
    pub oracle_resolution: u32,
    pub twap_records: u32,
    pub usdc: Address,
    pub xlm: Address,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Config,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[contracterror]
#[repr(u32)]
pub enum BackstopValuationError {
    AlreadyInitialized = 1600,
    InvalidConfiguration = 1601,
    InvalidOracle = 1602,
    InvalidComet = 1603,
    InvalidAmount = 1604,
    UnsupportedAsset = 1605,
    InvalidPrice = 1606,
    StalePrice = 1607,
    InvalidReserve = 1608,
    ArithmeticError = 1609,
}

#[contractclient(name = "Sep40Client")]
#[allow(dead_code)]
trait Sep40 {
    fn base(env: Env) -> OracleAsset;
    fn assets(env: Env) -> Vec<OracleAsset>;
    fn decimals(env: Env) -> u32;
    fn resolution(env: Env) -> u32;
    fn prices(env: Env, asset: OracleAsset, records: u32) -> Option<Vec<PriceData>>;
}

#[contractclient(name = "CometClient")]
#[allow(dead_code)]
trait Comet {
    fn get_tokens(env: Env) -> Vec<Address>;
    fn get_balance(env: Env, token: Address) -> i128;
    fn get_total_supply(env: Env) -> i128;
    fn get_normalized_weight(env: Env, token: Address) -> i128;
}

#[contractclient(name = "TokenInfoClient")]
#[allow(dead_code)]
trait TokenInfo {
    fn decimals(env: Env) -> u32;
}

#[contract]
pub struct BackstopValuation;

#[contractimpl]
impl BackstopValuation {
    #[allow(clippy::too_many_arguments)]
    pub fn __constructor(
        env: Env,
        oracle: Address,
        oracle_base: OracleAsset,
        blnd: Address,
        usdc: Address,
        xlm: Address,
        blnd_usdc: Address,
        blnd_xlm: Address,
        twap_records: u32,
        max_price_age: u64,
    ) {
        if env.storage().instance().has(&DataKey::Config) {
            panic_with_error!(&env, BackstopValuationError::AlreadyInitialized);
        }
        validate_distinct_addresses(&env, &oracle, &blnd, &usdc, &xlm, &blnd_usdc, &blnd_xlm);

        let oracle_client = Sep40Client::new(&env, &oracle);
        let oracle_decimals = oracle_client.decimals();
        let oracle_resolution = oracle_client.resolution();
        validate_oracle_configuration(
            &env,
            twap_records,
            max_price_age,
            oracle_decimals,
            oracle_resolution,
        );
        if oracle_base != OracleAsset::Stellar(usdc.clone()) || oracle_client.base() != oracle_base
        {
            panic_with_error!(&env, BackstopValuationError::InvalidOracle);
        }
        let oracle_assets = oracle_client.assets();
        if !oracle_assets.contains(OracleAsset::Stellar(blnd.clone()))
            || !oracle_assets.contains(OracleAsset::Stellar(xlm.clone()))
        {
            panic_with_error!(&env, BackstopValuationError::InvalidOracle);
        }

        validate_token_decimals(&env, &blnd);
        validate_token_decimals(&env, &usdc);
        validate_token_decimals(&env, &xlm);
        validate_token_decimals(&env, &blnd_usdc);
        validate_token_decimals(&env, &blnd_xlm);
        validate_comet(&env, &blnd_usdc, &blnd, &usdc);
        validate_comet(&env, &blnd_xlm, &blnd, &xlm);

        let config = AdapterConfig {
            blnd,
            blnd_usdc,
            blnd_xlm,
            max_price_age,
            oracle,
            oracle_base,
            oracle_decimals,
            oracle_resolution,
            twap_records,
            usdc,
            xlm,
        };
        env.storage().instance().set(&DataKey::Config, &config);
        extend_instance_ttl(&env);
    }

    pub fn version(env: Env) -> u32 {
        extend_instance_ttl(&env);
        ADAPTER_VERSION
    }

    pub fn binding(env: Env) -> AdapterBinding {
        extend_instance_ttl(&env);
        let config = read_config(&env);
        AdapterBinding {
            blnd: config.blnd,
            blnd_usdc: config.blnd_usdc,
            blnd_xlm: config.blnd_xlm,
            usdc: config.usdc,
        }
    }

    pub fn config(env: Env) -> AdapterConfig {
        extend_instance_ttl(&env);
        read_config(&env)
    }

    pub fn quote(env: Env, token: Address, amount: i128) -> AssetValuation {
        extend_instance_ttl(&env);
        if amount <= 0 {
            panic_with_error!(&env, BackstopValuationError::InvalidAmount);
        }
        let config = read_config(&env);
        verify_oracle_identity(&env, &config);
        if token == config.blnd_usdc {
            quote_comet(&env, &config, &token, &config.usdc, amount, None)
        } else if token == config.blnd_xlm {
            let xlm_price = read_twap(&env, &config, &config.xlm);
            quote_comet(&env, &config, &token, &config.xlm, amount, Some(xlm_price))
        } else {
            panic_with_error!(&env, BackstopValuationError::UnsupportedAsset);
        }
    }
}

#[derive(Clone)]
struct Twap {
    price: i128,
    valid_until: u64,
}

fn validate_distinct_addresses(
    env: &Env,
    oracle: &Address,
    blnd: &Address,
    usdc: &Address,
    xlm: &Address,
    blnd_usdc: &Address,
    blnd_xlm: &Address,
) {
    let addresses = [oracle, blnd, usdc, xlm, blnd_usdc, blnd_xlm];
    for (index, address) in addresses.iter().enumerate() {
        if addresses
            .iter()
            .skip(index + 1)
            .any(|other| address == other)
        {
            panic_with_error!(env, BackstopValuationError::InvalidConfiguration);
        }
    }
}

fn validate_oracle_configuration(
    env: &Env,
    twap_records: u32,
    max_price_age: u64,
    oracle_decimals: u32,
    oracle_resolution: u32,
) {
    if !(MIN_TWAP_RECORDS..=MAX_TWAP_RECORDS).contains(&twap_records)
        || oracle_decimals > 18
        || oracle_resolution == 0
        || max_price_age < u64::from(oracle_resolution)
        || max_price_age > MAX_PRICE_AGE_SECONDS
    {
        panic_with_error!(env, BackstopValuationError::InvalidConfiguration);
    }
    let history_span = u64::from(oracle_resolution)
        .checked_mul(u64::from(twap_records - 1))
        .unwrap_or_else(|| panic_with_error!(env, BackstopValuationError::ArithmeticError));
    if !(MIN_TWAP_WINDOW_SECONDS..=MAX_TWAP_WINDOW_SECONDS).contains(&history_span) {
        panic_with_error!(env, BackstopValuationError::InvalidConfiguration);
    }
}

fn validate_token_decimals(env: &Env, token: &Address) {
    if TokenInfoClient::new(env, token).decimals() != TOKEN_DECIMALS {
        panic_with_error!(env, BackstopValuationError::InvalidConfiguration);
    }
}

fn validate_comet(env: &Env, comet: &Address, blnd: &Address, pair: &Address) {
    let client = CometClient::new(env, comet);
    let tokens = client.get_tokens();
    if tokens.len() != 2 || !tokens.contains(blnd) || !tokens.contains(pair) {
        panic_with_error!(env, BackstopValuationError::InvalidComet);
    }
    if client.get_normalized_weight(blnd) != BLND_WEIGHT
        || client.get_normalized_weight(pair) != PAIR_WEIGHT
    {
        panic_with_error!(env, BackstopValuationError::InvalidComet);
    }
}

fn verify_oracle_identity(env: &Env, config: &AdapterConfig) {
    let client = Sep40Client::new(env, &config.oracle);
    if client.base() != config.oracle_base
        || client.decimals() != config.oracle_decimals
        || client.resolution() != config.oracle_resolution
    {
        panic_with_error!(env, BackstopValuationError::InvalidOracle);
    }
}

fn read_twap(env: &Env, config: &AdapterConfig, asset: &Address) -> Twap {
    let prices = Sep40Client::new(env, &config.oracle)
        .prices(&OracleAsset::Stellar(asset.clone()), &config.twap_records)
        .unwrap_or_else(|| panic_with_error!(env, BackstopValuationError::InvalidPrice));
    if prices.len() != config.twap_records {
        panic_with_error!(env, BackstopValuationError::InvalidPrice);
    }

    let now = env.ledger().timestamp();
    let resolution = u64::from(config.oracle_resolution);
    let expected_span = resolution
        .checked_mul(u64::from(config.twap_records - 1))
        .unwrap_or_else(|| panic_with_error!(env, BackstopValuationError::ArithmeticError));
    let mut timestamps = Map::<u64, bool>::new(env);
    let mut minimum_timestamp = u64::MAX;
    let mut maximum_timestamp = 0;
    let mut sum = I256::from_i32(env, 0);
    for datum in prices.iter() {
        if datum.price <= 0
            || datum.timestamp > now
            || datum.timestamp % resolution != 0
            || timestamps.contains_key(datum.timestamp)
        {
            panic_with_error!(env, BackstopValuationError::InvalidPrice);
        }
        timestamps.set(datum.timestamp, true);
        minimum_timestamp = minimum_timestamp.min(datum.timestamp);
        maximum_timestamp = maximum_timestamp.max(datum.timestamp);
        sum = sum.add(&I256::from_i128(env, datum.price));
    }
    if maximum_timestamp
        .checked_sub(minimum_timestamp)
        .filter(|span| *span == expected_span)
        .is_none()
    {
        panic_with_error!(env, BackstopValuationError::InvalidPrice);
    }
    let valid_until = maximum_timestamp
        .checked_add(config.max_price_age)
        .unwrap_or_else(|| panic_with_error!(env, BackstopValuationError::ArithmeticError));
    if now > valid_until {
        panic_with_error!(env, BackstopValuationError::StalePrice);
    }
    let price = sum
        .div(&I256::from_i128(env, i128::from(config.twap_records)))
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(env, BackstopValuationError::ArithmeticError));
    if price <= 0 {
        panic_with_error!(env, BackstopValuationError::InvalidPrice);
    }
    Twap { price, valid_until }
}

fn quote_comet(
    env: &Env,
    config: &AdapterConfig,
    comet: &Address,
    pair: &Address,
    amount: i128,
    pair_twap: Option<Twap>,
) -> AssetValuation {
    let comet_client = CometClient::new(env, comet);
    let total_supply = comet_client.get_total_supply();
    let blnd_reserve = comet_client.get_balance(&config.blnd);
    let pair_reserve = comet_client.get_balance(pair);
    if total_supply <= 0
        || amount > total_supply
        || blnd_reserve <= 0
        || pair_reserve <= 0
        || comet_client.get_normalized_weight(&config.blnd) != BLND_WEIGHT
        || comet_client.get_normalized_weight(pair) != PAIR_WEIGHT
    {
        panic_with_error!(env, BackstopValuationError::InvalidReserve);
    }

    let blnd_twap = read_twap(env, config, &config.blnd);
    let blnd_reserve_value = mul_div_floor(
        env,
        blnd_reserve,
        blnd_twap.price,
        ten_to_power(env, config.oracle_decimals),
    );
    let (pair_price, pair_valid_until) = pair_twap
        .map(|twap| (twap.price, twap.valid_until))
        .unwrap_or((SCALAR_7, u64::MAX));
    let pair_reserve_value = mul_div_floor(
        env,
        pair_reserve,
        pair_price,
        if pair == &config.usdc {
            SCALAR_7
        } else {
            ten_to_power(env, config.oracle_decimals)
        },
    );
    // At the immutable 80:20 target, either reserve independently implies the
    // same total LP value. Taking the lesser implication prevents a one-sided
    // reserve donation or temporary imbalance from inflating the quote.
    let blnd_implied_total = mul_div_floor(env, blnd_reserve_value, SCALAR_7, BLND_WEIGHT);
    let pair_implied_total = mul_div_floor(env, pair_reserve_value, SCALAR_7, PAIR_WEIGHT);
    let conservative_total = blnd_implied_total.min(pair_implied_total);
    let usdc_value = mul_div_floor(env, conservative_total, amount, total_supply);
    let underlying_blnd = mul_div_floor(env, blnd_reserve, amount, total_supply);

    AssetValuation {
        underlying_blnd,
        usdc_value,
        valid_until: blnd_twap.valid_until.min(pair_valid_until),
    }
}

fn ten_to_power(env: &Env, exponent: u32) -> i128 {
    let mut value = 1_i128;
    for _ in 0..exponent {
        value = value
            .checked_mul(10)
            .unwrap_or_else(|| panic_with_error!(env, BackstopValuationError::ArithmeticError));
    }
    value
}

fn mul_div_floor(env: &Env, value: i128, numerator: i128, denominator: i128) -> i128 {
    if value < 0 || numerator < 0 || denominator <= 0 {
        panic_with_error!(env, BackstopValuationError::ArithmeticError);
    }
    I256::from_i128(env, value)
        .mul(&I256::from_i128(env, numerator))
        .div(&I256::from_i128(env, denominator))
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(env, BackstopValuationError::ArithmeticError))
}

fn read_config(env: &Env) -> AdapterConfig {
    env.storage()
        .instance()
        .get(&DataKey::Config)
        .unwrap_or_else(|| panic_with_error!(env, BackstopValuationError::InvalidConfiguration))
}

fn extend_instance_ttl(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use soroban_sdk::{
        testutils::{storage::Instance as _, Address as _, Ledger},
        vec, Address,
    };

    #[derive(Clone)]
    #[contracttype]
    enum MockOracleKey {
        Assets,
        Base,
        Decimals,
        Price(Address),
        Resolution,
    }

    #[contract]
    struct MockOracle;

    #[contractimpl]
    impl MockOracle {
        pub fn __constructor(
            env: Env,
            base: OracleAsset,
            assets: Vec<OracleAsset>,
            decimals: u32,
            resolution: u32,
        ) {
            env.storage().instance().set(&MockOracleKey::Base, &base);
            env.storage()
                .instance()
                .set(&MockOracleKey::Assets, &assets);
            env.storage()
                .instance()
                .set(&MockOracleKey::Decimals, &decimals);
            env.storage()
                .instance()
                .set(&MockOracleKey::Resolution, &resolution);
        }

        pub fn base(env: Env) -> OracleAsset {
            env.storage().instance().get(&MockOracleKey::Base).unwrap()
        }

        pub fn assets(env: Env) -> Vec<OracleAsset> {
            env.storage()
                .instance()
                .get(&MockOracleKey::Assets)
                .unwrap()
        }

        pub fn decimals(env: Env) -> u32 {
            env.storage()
                .instance()
                .get(&MockOracleKey::Decimals)
                .unwrap()
        }

        pub fn resolution(env: Env) -> u32 {
            env.storage()
                .instance()
                .get(&MockOracleKey::Resolution)
                .unwrap()
        }

        pub fn prices(env: Env, asset: OracleAsset, _records: u32) -> Option<Vec<PriceData>> {
            let OracleAsset::Stellar(address) = asset else {
                panic!("unsupported mock asset");
            };
            env.storage().instance().get(&MockOracleKey::Price(address))
        }

        pub fn set_prices(env: Env, asset: Address, prices: Vec<PriceData>) {
            env.storage()
                .instance()
                .set(&MockOracleKey::Price(asset), &prices);
        }

        pub fn clear_prices(env: Env, asset: Address) {
            env.storage()
                .instance()
                .remove(&MockOracleKey::Price(asset));
        }

        pub fn set_resolution(env: Env, resolution: u32) {
            env.storage()
                .instance()
                .set(&MockOracleKey::Resolution, &resolution);
        }
    }

    #[derive(Clone)]
    #[contracttype]
    enum MockCometKey {
        Blnd,
        BlndReserve,
        BlndWeight,
        Pair,
        PairReserve,
        PairWeight,
        TotalSupply,
    }

    #[contract]
    struct MockComet;

    #[contractimpl]
    impl MockComet {
        #[allow(clippy::too_many_arguments)]
        pub fn __constructor(
            env: Env,
            blnd: Address,
            pair: Address,
            total_supply: i128,
            blnd_reserve: i128,
            pair_reserve: i128,
            blnd_weight: i128,
            pair_weight: i128,
        ) {
            env.storage().instance().set(&MockCometKey::Blnd, &blnd);
            env.storage().instance().set(&MockCometKey::Pair, &pair);
            env.storage()
                .instance()
                .set(&MockCometKey::TotalSupply, &total_supply);
            env.storage()
                .instance()
                .set(&MockCometKey::BlndReserve, &blnd_reserve);
            env.storage()
                .instance()
                .set(&MockCometKey::PairReserve, &pair_reserve);
            env.storage()
                .instance()
                .set(&MockCometKey::BlndWeight, &blnd_weight);
            env.storage()
                .instance()
                .set(&MockCometKey::PairWeight, &pair_weight);
        }

        pub fn decimals(_env: Env) -> u32 {
            TOKEN_DECIMALS
        }

        pub fn get_tokens(env: Env) -> Vec<Address> {
            vec![
                &env,
                env.storage().instance().get(&MockCometKey::Blnd).unwrap(),
                env.storage().instance().get(&MockCometKey::Pair).unwrap(),
            ]
        }

        pub fn get_balance(env: Env, token: Address) -> i128 {
            let blnd: Address = env.storage().instance().get(&MockCometKey::Blnd).unwrap();
            let key = if token == blnd {
                MockCometKey::BlndReserve
            } else {
                MockCometKey::PairReserve
            };
            env.storage().instance().get(&key).unwrap()
        }

        pub fn get_total_supply(env: Env) -> i128 {
            env.storage()
                .instance()
                .get(&MockCometKey::TotalSupply)
                .unwrap()
        }

        pub fn get_normalized_weight(env: Env, token: Address) -> i128 {
            let blnd: Address = env.storage().instance().get(&MockCometKey::Blnd).unwrap();
            let key = if token == blnd {
                MockCometKey::BlndWeight
            } else {
                MockCometKey::PairWeight
            };
            env.storage().instance().get(&key).unwrap()
        }

        pub fn set_reserves(env: Env, blnd_reserve: i128, pair_reserve: i128) {
            env.storage()
                .instance()
                .set(&MockCometKey::BlndReserve, &blnd_reserve);
            env.storage()
                .instance()
                .set(&MockCometKey::PairReserve, &pair_reserve);
        }
    }

    struct Fixture {
        adapter: Address,
        blnd: Address,
        blnd_usdc: Address,
        blnd_xlm: Address,
        env: Env,
        oracle: Address,
        usdc: Address,
        xlm: Address,
    }

    impl Fixture {
        fn new() -> Self {
            let env = Env::default();
            env.mock_all_auths();
            env.ledger().set_timestamp(3_600);
            let blnd = register_token(&env);
            let usdc = register_token(&env);
            let xlm = register_token(&env);
            let blnd_usdc = env.register(
                MockComet,
                (
                    &blnd,
                    &usdc,
                    &(100 * SCALAR_7),
                    &(1_600 * SCALAR_7),
                    &(20 * SCALAR_7),
                    &BLND_WEIGHT,
                    &PAIR_WEIGHT,
                ),
            );
            let blnd_xlm = env.register(
                MockComet,
                (
                    &blnd,
                    &xlm,
                    &(100 * SCALAR_7),
                    &(1_600 * SCALAR_7),
                    &(200 * SCALAR_7),
                    &BLND_WEIGHT,
                    &PAIR_WEIGHT,
                ),
            );
            let base = OracleAsset::Stellar(usdc.clone());
            let oracle = env.register(
                MockOracle,
                (
                    &base,
                    &vec![
                        &env,
                        OracleAsset::Stellar(blnd.clone()),
                        OracleAsset::Stellar(xlm.clone()),
                    ],
                    &7_u32,
                    &300_u32,
                ),
            );
            let oracle_client = MockOracleClient::new(&env, &oracle);
            oracle_client.set_prices(&blnd, &uniform_prices(&env, 500_000));
            oracle_client.set_prices(&xlm, &uniform_prices(&env, 1_000_000));
            let adapter_id = env.register(
                BackstopValuation,
                (
                    &oracle, &base, &blnd, &usdc, &xlm, &blnd_usdc, &blnd_xlm, &7_u32, &600_u64,
                ),
            );
            Self {
                adapter: adapter_id,
                blnd,
                blnd_usdc,
                blnd_xlm,
                env,
                oracle,
                usdc,
                xlm,
            }
        }

        fn adapter(&self) -> BackstopValuationClient<'_> {
            BackstopValuationClient::new(&self.env, &self.adapter)
        }
    }

    fn register_token(env: &Env) -> Address {
        env.register_stellar_asset_contract_v2(Address::generate(env))
            .address()
    }

    fn uniform_prices(env: &Env, price: i128) -> Vec<PriceData> {
        let mut prices = Vec::new(env);
        for index in 0_u64..7 {
            prices.push_back(PriceData {
                price,
                timestamp: 1_800 + index * 300,
            });
        }
        prices
    }

    #[test]
    fn quotes_balanced_lp_tokens_from_twap_and_current_composition() {
        let fixture = Fixture::new();
        assert_eq!(fixture.adapter().version(), ADAPTER_VERSION);
        assert_eq!(
            fixture.adapter().binding(),
            AdapterBinding {
                blnd: fixture.blnd.clone(),
                blnd_usdc: fixture.blnd_usdc.clone(),
                blnd_xlm: fixture.blnd_xlm.clone(),
                usdc: fixture.usdc.clone(),
            }
        );
        let config = fixture.adapter().config();
        assert_eq!(config.oracle, fixture.oracle);
        assert_eq!(
            config.oracle_base,
            OracleAsset::Stellar(fixture.usdc.clone())
        );
        assert_eq!(config.blnd, fixture.blnd);
        assert_eq!(config.xlm, fixture.xlm);

        assert_eq!(
            fixture
                .adapter()
                .quote(&fixture.blnd_usdc, &(10 * SCALAR_7)),
            AssetValuation {
                underlying_blnd: 160 * SCALAR_7,
                usdc_value: 10 * SCALAR_7,
                valid_until: 4_200,
            }
        );
        assert_eq!(
            fixture.adapter().quote(&fixture.blnd_xlm, &(10 * SCALAR_7)),
            AssetValuation {
                underlying_blnd: 160 * SCALAR_7,
                usdc_value: 10 * SCALAR_7,
                valid_until: 4_200,
            }
        );
    }

    #[test]
    fn version_only_usage_refreshes_instance_and_code_ttl() {
        let fixture = Fixture::new();
        fixture.env.as_contract(&fixture.adapter, || {
            assert_eq!(
                fixture.env.storage().instance().get_ttl(),
                INSTANCE_TTL_BUMP
            );
        });

        fixture
            .env
            .ledger()
            .set_sequence_number(fixture.env.ledger().sequence() + 2 * DAY_IN_LEDGERS);
        fixture.env.as_contract(&fixture.adapter, || {
            assert_eq!(
                fixture.env.storage().instance().get_ttl(),
                INSTANCE_TTL_BUMP - 2 * DAY_IN_LEDGERS
            );
        });

        assert_eq!(fixture.adapter().version(), ADAPTER_VERSION);
        fixture.env.as_contract(&fixture.adapter, || {
            assert_eq!(
                fixture.env.storage().instance().get_ttl(),
                INSTANCE_TTL_BUMP
            );
        });
    }

    #[test]
    fn twap_accepts_unordered_ticks_and_floors_the_average() {
        let fixture = Fixture::new();
        let mut prices = Vec::new(&fixture.env);
        for (timestamp, price) in [
            (3_600, 500_006),
            (1_800, 500_000),
            (3_000, 500_004),
            (2_100, 500_001),
            (2_700, 500_003),
            (2_400, 500_002),
            (3_300, 500_005),
        ] {
            prices.push_back(PriceData { price, timestamp });
        }
        MockOracleClient::new(&fixture.env, &fixture.oracle).set_prices(&fixture.blnd, &prices);
        let quote = fixture
            .adapter()
            .quote(&fixture.blnd_usdc, &(10 * SCALAR_7));
        assert_eq!(quote.usdc_value, 10 * SCALAR_7);
        assert_eq!(quote.valid_until, 4_200);
    }

    #[test]
    fn one_sided_reserve_increases_cannot_inflate_value() {
        let fixture = Fixture::new();
        let comet = MockCometClient::new(&fixture.env, &fixture.blnd_usdc);
        comet.set_reserves(&(3_200 * SCALAR_7), &(20 * SCALAR_7));
        assert_eq!(
            fixture
                .adapter()
                .quote(&fixture.blnd_usdc, &(10 * SCALAR_7))
                .usdc_value,
            10 * SCALAR_7
        );
        comet.set_reserves(&(1_600 * SCALAR_7), &(40 * SCALAR_7));
        assert_eq!(
            fixture
                .adapter()
                .quote(&fixture.blnd_usdc, &(10 * SCALAR_7))
                .usdc_value,
            10 * SCALAR_7
        );
        comet.set_reserves(&(800 * SCALAR_7), &(20 * SCALAR_7));
        assert_eq!(
            fixture
                .adapter()
                .quote(&fixture.blnd_usdc, &(10 * SCALAR_7))
                .usdc_value,
            5 * SCALAR_7
        );
    }

    #[test]
    fn malformed_missing_stale_and_changed_oracle_data_fail_closed() {
        let fixture = Fixture::new();
        fixture.env.ledger().set_timestamp(4_201);
        assert!(fixture
            .adapter()
            .try_quote(&fixture.blnd_usdc, &(10 * SCALAR_7))
            .is_err());

        fixture.env.ledger().set_timestamp(3_600);
        let oracle = MockOracleClient::new(&fixture.env, &fixture.oracle);
        let mut gapped = uniform_prices(&fixture.env, 500_000);
        gapped.set(
            3,
            PriceData {
                price: 500_000,
                timestamp: 2_500,
            },
        );
        oracle.set_prices(&fixture.blnd, &gapped);
        assert!(fixture
            .adapter()
            .try_quote(&fixture.blnd_usdc, &(10 * SCALAR_7))
            .is_err());

        let mut duplicate = uniform_prices(&fixture.env, 500_000);
        duplicate.set(
            4,
            PriceData {
                price: 500_000,
                timestamp: 2_700,
            },
        );
        oracle.set_prices(&fixture.blnd, &duplicate);
        assert!(fixture
            .adapter()
            .try_quote(&fixture.blnd_usdc, &(10 * SCALAR_7))
            .is_err());

        let mut nonpositive = uniform_prices(&fixture.env, 500_000);
        nonpositive.set(
            0,
            PriceData {
                price: 0,
                timestamp: 1_800,
            },
        );
        oracle.set_prices(&fixture.blnd, &nonpositive);
        assert!(fixture
            .adapter()
            .try_quote(&fixture.blnd_usdc, &(10 * SCALAR_7))
            .is_err());

        let mut future = Vec::new(&fixture.env);
        for index in 0_u64..7 {
            future.push_back(PriceData {
                price: 500_000,
                timestamp: 2_100 + index * 300,
            });
        }
        oracle.set_prices(&fixture.blnd, &future);
        assert!(fixture
            .adapter()
            .try_quote(&fixture.blnd_usdc, &(10 * SCALAR_7))
            .is_err());

        oracle.clear_prices(&fixture.blnd);
        assert!(fixture
            .adapter()
            .try_quote(&fixture.blnd_usdc, &(10 * SCALAR_7))
            .is_err());

        oracle.set_prices(&fixture.blnd, &uniform_prices(&fixture.env, 500_000));
        oracle.set_resolution(&301);
        assert!(fixture
            .adapter()
            .try_quote(&fixture.blnd_usdc, &(10 * SCALAR_7))
            .is_err());
    }

    #[test]
    fn invalid_amount_asset_and_reserves_fail_closed() {
        let fixture = Fixture::new();
        assert!(fixture.adapter().try_quote(&fixture.blnd_usdc, &0).is_err());
        assert!(fixture
            .adapter()
            .try_quote(&fixture.usdc, &(10 * SCALAR_7))
            .is_err());
        assert!(fixture
            .adapter()
            .try_quote(&fixture.blnd_usdc, &(101 * SCALAR_7))
            .is_err());
        MockCometClient::new(&fixture.env, &fixture.blnd_usdc).set_reserves(&0, &(20 * SCALAR_7));
        assert!(fixture
            .adapter()
            .try_quote(&fixture.blnd_usdc, &(10 * SCALAR_7))
            .is_err());
    }

    #[test]
    fn positive_dust_amount_can_round_to_zero_without_blocking_valuation() {
        let fixture = Fixture::new();
        MockCometClient::new(&fixture.env, &fixture.blnd_usdc)
            .set_reserves(&(1_600 * SCALAR_7), &1);
        assert_eq!(
            fixture.adapter().quote(&fixture.blnd_usdc, &1),
            AssetValuation {
                underlying_blnd: 16,
                usdc_value: 0,
                valid_until: 4_200,
            }
        );
    }

    #[test]
    #[should_panic]
    fn constructor_rejects_wrong_comet_weights() {
        let env = Env::default();
        register_constructor_fixture(&env, true, 7_000_000, 3_000_000);
    }

    #[test]
    #[should_panic]
    fn constructor_rejects_non_usdc_oracle_base() {
        let env = Env::default();
        register_constructor_fixture(&env, false, BLND_WEIGHT, PAIR_WEIGHT);
    }

    fn register_constructor_fixture(
        env: &Env,
        use_usdc_base: bool,
        blnd_weight: i128,
        pair_weight: i128,
    ) {
        env.ledger().set_timestamp(3_600);
        let blnd = register_token(env);
        let usdc = register_token(env);
        let xlm = register_token(env);
        let blnd_usdc = env.register(
            MockComet,
            (
                &blnd,
                &usdc,
                &(100 * SCALAR_7),
                &(1_600 * SCALAR_7),
                &(20 * SCALAR_7),
                &blnd_weight,
                &pair_weight,
            ),
        );
        let blnd_xlm = env.register(
            MockComet,
            (
                &blnd,
                &xlm,
                &(100 * SCALAR_7),
                &(1_600 * SCALAR_7),
                &(200 * SCALAR_7),
                &BLND_WEIGHT,
                &PAIR_WEIGHT,
            ),
        );
        let base = if use_usdc_base {
            OracleAsset::Stellar(usdc.clone())
        } else {
            OracleAsset::Other(Symbol::new(env, "USD"))
        };
        let oracle = env.register(
            MockOracle,
            (
                &base,
                &vec![
                    env,
                    OracleAsset::Stellar(blnd.clone()),
                    OracleAsset::Stellar(xlm.clone()),
                ],
                &7_u32,
                &300_u32,
            ),
        );
        env.register(
            BackstopValuation,
            (
                &oracle, &base, &blnd, &usdc, &xlm, &blnd_usdc, &blnd_xlm, &7_u32, &600_u64,
            ),
        );
    }
}
