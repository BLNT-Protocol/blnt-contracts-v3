#![cfg(test)]

use crate::{
    constants::{SCALAR_12, SCALAR_7},
    pool::Reserve,
    storage::{self, ReserveConfig, ReserveData},
    PoolContract,
};
use mock_emitter::MockEmitter;
use sep_40_oracle::testutils::{MockPriceOracle, MockPriceOracleClient};
use sep_41_token::testutils::{MockToken, MockTokenClient};
use soroban_fixed_point_math::SorobanFixedPoint;
use soroban_sdk::{
    contract, contractimpl, contracttype, testutils::Address as _, vec, Address, BytesN, Env,
    IntoVal, String,
};

use backstop::{BackstopClient, BackstopContract, EmitterClient};
use mock_pool_factory::{
    BackstopAsset, BackstopTierConfig, MockPoolFactory, MockPoolFactoryClient, PoolInitMeta,
};
use moderc3156_example::{
    FlashLoanReceiverModifiedERC3156, FlashLoanReceiverModifiedERC3156Client,
};

/// Create a pool contract.
///
/// This sets random data in the constructor, so unit tests that
/// rely on any constructor data need to reset it.
pub(crate) fn create_pool(e: &Env) -> Address {
    create_pool_with_access_controller(e, None)
}

pub(crate) fn create_pool_with_access_controller(
    e: &Env,
    access_controller: Option<Address>,
) -> Address {
    e.register(
        PoolContract {},
        (
            Address::generate(e),
            String::from_str(e, "teapot"),
            Address::generate(e),
            0_1000000u32,
            4u32,
            1_0000000i128,
            Address::generate(e),
            Address::generate(e),
            access_controller,
        ),
    )
}

#[derive(Clone)]
#[contracttype]
enum MockAccessControllerKey {
    Permissions(Address, Address),
    Fail,
}

#[contract]
pub(crate) struct MockAccessController;

#[contractimpl]
impl MockAccessController {
    pub fn permissions(e: Env, pool: Address, user: Address) -> u32 {
        if e.storage()
            .instance()
            .get(&MockAccessControllerKey::Fail)
            .unwrap_or(false)
        {
            panic!("controller unavailable");
        }
        e.storage()
            .persistent()
            .get(&MockAccessControllerKey::Permissions(pool, user))
            .unwrap_or(0)
    }

    pub fn set_permissions(e: Env, pool: Address, user: Address, permissions: u32) {
        e.storage().persistent().set(
            &MockAccessControllerKey::Permissions(pool, user),
            &permissions,
        );
    }

    pub fn set_fail(e: Env, fail: bool) {
        e.storage()
            .instance()
            .set(&MockAccessControllerKey::Fail, &fail);
    }
}

//************************************************
//           External Contract Helpers
//************************************************

// ***** Token *****

pub(crate) fn create_token_contract<'a>(
    e: &Env,
    admin: &Address,
) -> (Address, MockTokenClient<'a>) {
    let contract_address = Address::generate(e);
    e.register_at(&contract_address, MockToken, ());
    let client = MockTokenClient::new(e, &contract_address);
    client.initialize(admin, &7, &"unit".into_val(e), &"test".into_val(e));
    (contract_address, client)
}

pub(crate) fn create_blnt_token<'a>(
    e: &Env,
    pool_address: &Address,
    admin: &Address,
) -> (Address, MockTokenClient<'a>) {
    let (contract_address, client) = create_token_contract(e, admin);

    e.as_contract(pool_address, || {
        storage::set_blnt_token(e, &contract_address);
    });
    (contract_address, client)
}

//***** Oracle ******

pub(crate) fn create_mock_oracle(e: &Env) -> (Address, MockPriceOracleClient<'_>) {
    let contract_address = e.register(MockPriceOracle, ());
    (
        contract_address.clone(),
        MockPriceOracleClient::new(e, &contract_address),
    )
}

//***** Pool Factory ******

pub(crate) fn create_mock_pool_factory<'a>(
    e: &'a Env,
    backstop: &Address,
) -> (Address, MockPoolFactoryClient<'a>) {
    let pool_init_meta = PoolInitMeta {
        backstop: backstop.clone(),
        pool_hash: BytesN::<32>::from_array(&e, &[0u8; 32]),
        blnt_id: Address::generate(e),
    };
    let contract_address = e.register(MockPoolFactory {}, (pool_init_meta,));
    (
        contract_address.clone(),
        MockPoolFactoryClient::new(e, &contract_address),
    )
}

//***** Pool Factory ******

pub(crate) fn create_emitter<'a>(
    e: &Env,
    backstop_id: &Address,
    backstop_token: &Address,
    blnt_token: &Address,
) -> (Address, EmitterClient<'a>) {
    let contract_address = e.register(MockEmitter, ());
    let client = EmitterClient::new(e, &contract_address);
    client.initialize(blnt_token, backstop_id, backstop_token);
    (contract_address.clone(), client)
}

//***** Backstop ******

mod comet {
    soroban_sdk::contractimport!(file = "../comet.wasm");
}

pub(crate) fn create_backstop<'a>(
    e: &Env,
    pool_address: &Address,
    backstop_token: &Address,
    usdc_token: &Address,
    blnt_token: &Address,
) -> (Address, BackstopClient<'a>) {
    let backstop_id = Address::generate(e);
    let comet_admin = Address::generate(e);
    let (xlm_token, _) = create_token_contract(e, &comet_admin);
    let (blnt_xlm_token, _) = create_comet_lp_pool(e, &comet_admin, blnt_token, &xlm_token);
    let (pool_factory, mock_pool_factory_client) = create_mock_pool_factory(e, &backstop_id);
    mock_pool_factory_client.set_pool_config(
        pool_address,
        &vec![
            e,
            BackstopTierConfig {
                asset: BackstopAsset::BlntXlm,
                take_rate_weight: 4,
            },
            BackstopTierConfig {
                asset: BackstopAsset::BlntUsdc,
                take_rate_weight: 3,
            },
            BackstopTierConfig {
                asset: BackstopAsset::Usdc,
                take_rate_weight: 2,
            },
        ],
    );
    let (emitter, _) = create_emitter(e, &backstop_id, backstop_token, blnt_token);
    e.register_at(
        &backstop_id,
        BackstopContract {},
        (
            backstop_token,
            blnt_xlm_token,
            emitter,
            blnt_token,
            usdc_token,
            xlm_token,
            pool_factory,
            soroban_sdk::Vec::<(Address, i128)>::new(e),
        ),
    );
    e.as_contract(&backstop_id, || {
        backstop::set_test_valuation_override(e, Some(false));
        backstop::activate_for_test(e, e.ledger().timestamp());
    });
    e.as_contract(pool_address, || {
        storage::set_backstop(e, &backstop_id);
    });
    let client = BackstopClient::new(e, &backstop_id);
    (backstop_id.clone(), client)
}

/// Deploy a test Comet v2 LP pool of 80% BLNT / 20% USDC and set it as the backstop token.
///
/// Initializes the pool with the following settings:
/// - Swap fee: 0.3%
/// - BLNT: 1,000
/// - USDC: 25
/// - Shares: 100
pub(crate) fn create_comet_lp_pool<'a>(
    e: &Env,
    admin: &Address,
    blnt_token: &Address,
    usdc_token: &Address,
) -> (Address, comet::Client<'a>) {
    let contract_address = Address::generate(e);
    e.register_at(&contract_address, comet::WASM, ());
    let client = comet::Client::new(e, &contract_address);

    let blnt_client = MockTokenClient::new(e, blnt_token);
    let usdc_client = MockTokenClient::new(e, usdc_token);
    blnt_client.mint(&admin, &1_000_0000000);
    usdc_client.mint(&admin, &25_0000000);

    client.init(
        admin,
        &vec![e, blnt_token.clone(), usdc_token.clone()],
        &vec![e, 0_8000000, 0_2000000],
        &vec![e, 1_000_0000000, 25_0000000],
        &0_0030000,
    );

    (contract_address, client)
}

//***** Flash Loan *****

/// Create a flash loan receiver contract.
///
/// This returns the tokens received from the flash loan to the "caller" for
/// test purposes.
pub fn create_flashloan_receiver<'a>(
    e: &Env,
) -> (Address, FlashLoanReceiverModifiedERC3156Client<'a>) {
    let contract_id = Address::generate(e);
    e.register_at(&contract_id, FlashLoanReceiverModifiedERC3156 {}, ());

    (
        contract_id.clone(),
        FlashLoanReceiverModifiedERC3156Client::new(e, &contract_id),
    )
}

//************************************************
//            Object Creation Helpers
//************************************************

//***** Reserve *****

pub(crate) fn default_reserve(e: &Env) -> Reserve {
    Reserve {
        asset: Address::generate(e),
        config: ReserveConfig {
            decimals: 7,
            c_factor: 0_7500000,
            l_factor: 0_7500000,
            util: 0_7500000,
            max_util: 0_9500000,
            r_base: 0_0100000,
            r_one: 0_0500000,
            r_two: 0_5000000,
            r_three: 1_5000000,
            reactivity: 0_0000020, // 2e-6
            index: 0,
            supply_cap: 1000000000000000000,
            enabled: true,
        },
        data: ReserveData {
            b_rate: SCALAR_12,
            d_rate: SCALAR_12,
            ir_mod: SCALAR_7,
            b_supply: 100_0000000,
            d_supply: 75_0000000,
            last_time: 0,
            backstop_credit: 0,
        },
        scalar: SCALAR_7,
    }
}

pub(crate) fn default_reserve_meta() -> (ReserveConfig, ReserveData) {
    (
        ReserveConfig {
            decimals: 7,
            c_factor: 0_7500000,
            l_factor: 0_7500000,
            util: 0_7500000,
            max_util: 0_9500000,
            r_base: 0_0100000,
            r_one: 0_0500000,
            r_two: 0_5000000,
            r_three: 1_5000000,
            reactivity: 0_0000020, // 2e-6
            index: 0,
            supply_cap: 1000000000000000000,
            enabled: true,
        },
        ReserveData {
            b_rate: SCALAR_12,
            d_rate: SCALAR_12,
            ir_mod: SCALAR_7,
            b_supply: 100_0000000,
            d_supply: 75_0000000,
            last_time: 0,
            backstop_credit: 0,
        },
    )
}

/// Create a reserve based on the supplied config and data.
///
/// Mints the appropriate amount of underlying tokens to the pool based on the
/// b and d token supply and rates.
///
/// Returns the underlying asset address.
pub(crate) fn create_reserve(
    e: &Env,
    pool_address: &Address,
    token_address: &Address,
    reserve_config: &ReserveConfig,
    reserve_data: &ReserveData,
) {
    let mut new_reserve_config = reserve_config.clone();
    e.as_contract(pool_address, || {
        let index = storage::push_res_list(e, &token_address);
        new_reserve_config.index = index;
        storage::set_res_config(e, &token_address, &new_reserve_config);
        storage::set_res_data(e, &token_address, &reserve_data);
    });
    let underlying_client = MockTokenClient::new(e, token_address);

    // mint pool assets to set expected b_rate
    let total_supply = reserve_data
        .b_supply
        .fixed_mul_floor(e, &reserve_data.b_rate, &SCALAR_12);
    let total_liabilities =
        reserve_data
            .d_supply
            .fixed_mul_floor(e, &reserve_data.d_rate, &SCALAR_12);
    let to_mint_pool = total_supply - total_liabilities + reserve_data.backstop_credit;
    underlying_client
        .mock_all_auths()
        .mint(&pool_address, &to_mint_pool);
}
