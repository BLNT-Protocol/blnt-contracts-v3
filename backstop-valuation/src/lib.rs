#![no_std]

use soroban_sdk::{
    contract, contractclient, contracterror, contractimpl, contracttype, panic_with_error, Address,
    Env, Vec, I256,
};

const BLND_WEIGHT: i128 = 8_000_000;
const PAIR_WEIGHT: i128 = 2_000_000;
const PAIR_VALUE_MULTIPLIER: i128 = 5;
const TOKEN_DECIMALS: u32 = 7;
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
pub struct AdapterConfig {
    pub blnd: Address,
    pub blnd_usdc: Address,
    pub blnd_xlm: Address,
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
    InvalidComet = 1603,
    InvalidAmount = 1604,
    UnsupportedAsset = 1605,
    InvalidReserve = 1608,
    ArithmeticError = 1609,
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
    pub fn __constructor(
        env: Env,
        blnd: Address,
        usdc: Address,
        xlm: Address,
        blnd_usdc: Address,
        blnd_xlm: Address,
    ) {
        if env.storage().instance().has(&DataKey::Config) {
            panic_with_error!(&env, BackstopValuationError::AlreadyInitialized);
        }
        validate_distinct_addresses(&env, &blnd, &usdc, &xlm, &blnd_usdc, &blnd_xlm);

        validate_token_decimals(&env, &blnd);
        validate_token_decimals(&env, &usdc);
        validate_token_decimals(&env, &xlm);
        validate_token_decimals(&env, &blnd_usdc);
        validate_token_decimals(&env, &blnd_xlm);
        validate_comet(&env, &blnd_usdc, &blnd, &usdc);
        validate_comet(&env, &blnd_xlm, &blnd, &xlm);

        env.storage().instance().set(
            &DataKey::Config,
            &AdapterConfig {
                blnd,
                blnd_usdc,
                blnd_xlm,
                usdc,
                xlm,
            },
        );
        extend_instance_ttl(&env);
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
        let anchor = read_comet(&env, &config.blnd_usdc, &config.blnd, &config.usdc);
        if token == config.blnd_usdc {
            let total_value = checked_mul(&env, anchor.pair_reserve, PAIR_VALUE_MULTIPLIER);
            quote_amount(&env, amount, total_value, &anchor)
        } else if token == config.blnd_xlm {
            let target = read_comet(&env, &config.blnd_xlm, &config.blnd, &config.xlm);
            let anchor_value = checked_mul(&env, anchor.pair_reserve, PAIR_VALUE_MULTIPLIER);
            let total_value =
                mul_div_floor(&env, target.blnd_reserve, anchor_value, anchor.blnd_reserve);
            quote_amount(&env, amount, total_value, &target)
        } else {
            panic_with_error!(&env, BackstopValuationError::UnsupportedAsset);
        }
    }
}

struct CometComposition {
    blnd_reserve: i128,
    pair_reserve: i128,
    total_supply: i128,
}

fn validate_distinct_addresses(
    env: &Env,
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
            panic_with_error!(env, BackstopValuationError::InvalidConfiguration);
        }
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

fn read_comet(env: &Env, comet: &Address, blnd: &Address, pair: &Address) -> CometComposition {
    let client = CometClient::new(env, comet);
    let total_supply = client.get_total_supply();
    let blnd_reserve = client.get_balance(blnd);
    let pair_reserve = client.get_balance(pair);
    if total_supply <= 0
        || blnd_reserve <= 0
        || pair_reserve <= 0
        || client.get_normalized_weight(blnd) != BLND_WEIGHT
        || client.get_normalized_weight(pair) != PAIR_WEIGHT
    {
        panic_with_error!(env, BackstopValuationError::InvalidReserve);
    }
    CometComposition {
        blnd_reserve,
        pair_reserve,
        total_supply,
    }
}

fn quote_amount(
    env: &Env,
    amount: i128,
    total_value: i128,
    composition: &CometComposition,
) -> AssetValuation {
    if amount > composition.total_supply || total_value <= 0 {
        panic_with_error!(env, BackstopValuationError::InvalidReserve);
    }
    AssetValuation {
        underlying_blnd: mul_div_floor(
            env,
            amount,
            composition.blnd_reserve,
            composition.total_supply,
        ),
        usdc_value: mul_div_floor(env, amount, total_value, composition.total_supply),
        valid_until: u64::MAX,
    }
}

fn checked_mul(env: &Env, left: i128, right: i128) -> i128 {
    left.checked_mul(right)
        .unwrap_or_else(|| panic_with_error!(env, BackstopValuationError::ArithmeticError))
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
        testutils::{storage::Instance as _, Address as _},
        vec, Address,
    };

    #[contract]
    struct MockToken;

    #[contractimpl]
    impl MockToken {
        pub fn __constructor(env: Env, decimals: u32) {
            env.storage().instance().set(&0_u32, &decimals);
        }

        pub fn decimals(env: Env) -> u32 {
            env.storage().instance().get(&0_u32).unwrap()
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
        Supply,
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
            blnd_reserve: i128,
            pair_reserve: i128,
            supply: i128,
            blnd_weight: i128,
            pair_weight: i128,
        ) {
            env.storage().instance().set(&MockCometKey::Blnd, &blnd);
            env.storage().instance().set(&MockCometKey::Pair, &pair);
            env.storage()
                .instance()
                .set(&MockCometKey::BlndReserve, &blnd_reserve);
            env.storage()
                .instance()
                .set(&MockCometKey::PairReserve, &pair_reserve);
            env.storage().instance().set(&MockCometKey::Supply, &supply);
            env.storage()
                .instance()
                .set(&MockCometKey::BlndWeight, &blnd_weight);
            env.storage()
                .instance()
                .set(&MockCometKey::PairWeight, &pair_weight);
        }

        pub fn get_tokens(env: Env) -> Vec<Address> {
            vec![
                &env,
                env.storage().instance().get(&MockCometKey::Blnd).unwrap(),
                env.storage().instance().get(&MockCometKey::Pair).unwrap(),
            ]
        }

        pub fn decimals(_env: Env) -> u32 {
            TOKEN_DECIMALS
        }

        pub fn get_balance(env: Env, token: Address) -> i128 {
            if token == env.storage().instance().get(&MockCometKey::Blnd).unwrap() {
                env.storage()
                    .instance()
                    .get(&MockCometKey::BlndReserve)
                    .unwrap()
            } else if token == env.storage().instance().get(&MockCometKey::Pair).unwrap() {
                env.storage()
                    .instance()
                    .get(&MockCometKey::PairReserve)
                    .unwrap()
            } else {
                0
            }
        }

        pub fn get_total_supply(env: Env) -> i128 {
            env.storage().instance().get(&MockCometKey::Supply).unwrap()
        }

        pub fn get_normalized_weight(env: Env, token: Address) -> i128 {
            if token == env.storage().instance().get(&MockCometKey::Blnd).unwrap() {
                env.storage()
                    .instance()
                    .get(&MockCometKey::BlndWeight)
                    .unwrap()
            } else if token == env.storage().instance().get(&MockCometKey::Pair).unwrap() {
                env.storage()
                    .instance()
                    .get(&MockCometKey::PairWeight)
                    .unwrap()
            } else {
                0
            }
        }

        pub fn set_reserves(env: Env, blnd_reserve: i128, pair_reserve: i128, supply: i128) {
            env.storage()
                .instance()
                .set(&MockCometKey::BlndReserve, &blnd_reserve);
            env.storage()
                .instance()
                .set(&MockCometKey::PairReserve, &pair_reserve);
            env.storage().instance().set(&MockCometKey::Supply, &supply);
        }
    }

    struct Fixture {
        adapter: Address,
        blnd: Address,
        blnd_usdc: Address,
        blnd_xlm: Address,
        env: Env,
        usdc: Address,
        xlm: Address,
    }

    impl Fixture {
        fn create() -> Self {
            let env = Env::default();
            env.cost_estimate().budget().reset_unlimited();
            let blnd = env.register(MockToken, (TOKEN_DECIMALS,));
            let usdc = env.register(MockToken, (TOKEN_DECIMALS,));
            let xlm = env.register(MockToken, (TOKEN_DECIMALS,));
            let blnd_usdc = env.register(
                MockComet,
                (
                    blnd.clone(),
                    usdc.clone(),
                    1_000 * 10_000_000_i128,
                    25 * 10_000_000_i128,
                    100 * 10_000_000_i128,
                    BLND_WEIGHT,
                    PAIR_WEIGHT,
                ),
            );
            let blnd_xlm = env.register(
                MockComet,
                (
                    blnd.clone(),
                    xlm.clone(),
                    500 * 10_000_000_i128,
                    100 * 10_000_000_i128,
                    50 * 10_000_000_i128,
                    BLND_WEIGHT,
                    PAIR_WEIGHT,
                ),
            );
            let adapter = env.register(
                BackstopValuation,
                (
                    blnd.clone(),
                    usdc.clone(),
                    xlm.clone(),
                    blnd_usdc.clone(),
                    blnd_xlm.clone(),
                ),
            );
            Self {
                adapter,
                blnd,
                blnd_usdc,
                blnd_xlm,
                env,
                usdc,
                xlm,
            }
        }

        fn client(&self) -> BackstopValuationClient<'_> {
            BackstopValuationClient::new(&self.env, &self.adapter)
        }
    }

    #[test]
    fn exposes_config_and_binding() {
        let fixture = Fixture::create();
        assert_eq!(
            fixture.client().binding(),
            AdapterBinding {
                blnd: fixture.blnd.clone(),
                blnd_usdc: fixture.blnd_usdc.clone(),
                blnd_xlm: fixture.blnd_xlm.clone(),
                usdc: fixture.usdc.clone(),
            }
        );
        assert_eq!(
            fixture.client().config(),
            AdapterConfig {
                blnd: fixture.blnd,
                blnd_usdc: fixture.blnd_usdc,
                blnd_xlm: fixture.blnd_xlm,
                usdc: fixture.usdc,
                xlm: fixture.xlm,
            }
        );
    }

    #[test]
    fn values_both_lp_tokens_from_comet_reserves() {
        let fixture = Fixture::create();
        assert_eq!(
            fixture
                .client()
                .quote(&fixture.blnd_usdc, &(20 * 10_000_000_i128)),
            AssetValuation {
                underlying_blnd: 200 * 10_000_000,
                usdc_value: 25 * 10_000_000,
                valid_until: u64::MAX,
            }
        );
        assert_eq!(
            fixture
                .client()
                .quote(&fixture.blnd_xlm, &(10 * 10_000_000_i128)),
            AssetValuation {
                underlying_blnd: 100 * 10_000_000,
                usdc_value: 125_000_000,
                valid_until: u64::MAX,
            }
        );
    }

    #[test]
    fn blnd_xlm_cross_value_tracks_blnd_usdc_spot_ratio() {
        let fixture = Fixture::create();
        MockCometClient::new(&fixture.env, &fixture.blnd_usdc).set_reserves(
            &(500 * 10_000_000_i128),
            &(25 * 10_000_000_i128),
            &(100 * 10_000_000_i128),
        );
        assert_eq!(
            fixture
                .client()
                .quote(&fixture.blnd_xlm, &(10 * 10_000_000_i128))
                .usdc_value,
            25 * 10_000_000
        );
    }

    #[test]
    fn proportionate_anchor_liquidity_preserves_value_per_lp() {
        let fixture = Fixture::create();
        let before = fixture
            .client()
            .quote(&fixture.blnd_usdc, &(10 * 10_000_000_i128));
        MockCometClient::new(&fixture.env, &fixture.blnd_usdc).set_reserves(
            &(2_000 * 10_000_000_i128),
            &(50 * 10_000_000_i128),
            &(200 * 10_000_000_i128),
        );
        assert_eq!(
            fixture
                .client()
                .quote(&fixture.blnd_usdc, &(10 * 10_000_000_i128)),
            before
        );
    }

    #[test]
    fn invalid_amount_asset_or_reserve_fails_closed() {
        let fixture = Fixture::create();
        assert!(fixture.client().try_quote(&fixture.blnd_usdc, &0).is_err());
        assert!(fixture
            .client()
            .try_quote(&Address::generate(&fixture.env), &1)
            .is_err());
        MockCometClient::new(&fixture.env, &fixture.blnd_usdc).set_reserves(
            &0,
            &(25 * 10_000_000_i128),
            &(100 * 10_000_000_i128),
        );
        assert!(fixture.client().try_quote(&fixture.blnd_usdc, &1).is_err());
    }

    #[test]
    #[should_panic]
    fn constructor_rejects_wrong_decimals() {
        let env = Env::default();
        env.cost_estimate().budget().reset_unlimited();
        let blnd = env.register(MockToken, (TOKEN_DECIMALS,));
        let usdc = env.register(MockToken, (6_u32,));
        let xlm = env.register(MockToken, (TOKEN_DECIMALS,));
        let blnd_usdc = env.register(
            MockComet,
            (
                blnd.clone(),
                usdc.clone(),
                1_i128,
                1_i128,
                1_i128,
                BLND_WEIGHT,
                PAIR_WEIGHT,
            ),
        );
        let blnd_xlm = env.register(
            MockComet,
            (
                blnd.clone(),
                xlm.clone(),
                1_i128,
                1_i128,
                1_i128,
                7_000_000_i128,
                3_000_000_i128,
            ),
        );
        env.register(BackstopValuation, (blnd, usdc, xlm, blnd_usdc, blnd_xlm));
    }

    #[test]
    #[should_panic]
    fn constructor_rejects_wrong_comet_weights() {
        let env = Env::default();
        env.cost_estimate().budget().reset_unlimited();
        let blnd = env.register(MockToken, (TOKEN_DECIMALS,));
        let usdc = env.register(MockToken, (TOKEN_DECIMALS,));
        let xlm = env.register(MockToken, (TOKEN_DECIMALS,));
        let blnd_usdc = env.register(
            MockComet,
            (
                blnd.clone(),
                usdc.clone(),
                1_i128,
                1_i128,
                1_i128,
                BLND_WEIGHT,
                PAIR_WEIGHT,
            ),
        );
        let blnd_xlm = env.register(
            MockComet,
            (
                blnd.clone(),
                xlm.clone(),
                1_i128,
                1_i128,
                1_i128,
                7_000_000_i128,
                3_000_000_i128,
            ),
        );
        env.register(BackstopValuation, (blnd, usdc, xlm, blnd_usdc, blnd_xlm));
    }

    #[test]
    fn public_reads_extend_instance_ttl() {
        let fixture = Fixture::create();
        fixture.env.as_contract(&fixture.adapter, || {
            fixture.env.storage().instance().extend_ttl(0, 1);
        });
        fixture.client().binding();
        fixture.env.as_contract(&fixture.adapter, || {
            assert!(fixture.env.storage().instance().get_ttl() >= INSTANCE_TTL_THRESHOLD);
        });
    }
}
