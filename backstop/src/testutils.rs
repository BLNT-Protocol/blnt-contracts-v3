#![cfg(test)]

use crate::{
    backstop::{set_test_valuation_override, Q4W},
    dependencies::{CometClient, EmitterClient, COMET_WASM},
    storage::{self},
    BackstopContract,
};

use mock_emitter::MockEmitter;
use mock_pool::MockPoolClient;
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{Address as _, Ledger, LedgerInfo},
    unwrap::UnwrapOptimized,
    vec, Address, BytesN, Env, IntoVal, Vec,
};

use mock_pool_factory::{MockPoolFactory, MockPoolFactoryClient, PoolInitMeta};
use sep_41_token::testutils::{MockToken, MockTokenClient};

#[derive(Clone)]
#[contracttype]
enum MockCometConfigKey {
    Tokens,
}

#[contract]
struct MockCometConfig;

#[contractimpl]
impl MockCometConfig {
    pub fn __constructor(e: Env, tokens: Vec<Address>) {
        e.storage()
            .instance()
            .set(&MockCometConfigKey::Tokens, &tokens);
    }

    pub fn decimals(_e: Env) -> u32 {
        7
    }

    pub fn get_tokens(e: Env) -> Vec<Address> {
        e.storage()
            .instance()
            .get(&MockCometConfigKey::Tokens)
            .unwrap()
    }

    pub fn get_normalized_weight(e: Env, token: Address) -> i128 {
        let tokens = Self::get_tokens(e);
        if tokens.get(0).unwrap() == token {
            8_000_000
        } else if tokens.get(1).unwrap() == token {
            2_000_000
        } else {
            0
        }
    }
}

/// Create a backstop contract.
///
/// Unit tests use an explicit one-to-one valuation override after the
/// constructor validates two minimal 80:20 Comet interfaces. Dedicated
/// valuation tests exercise the real Comet WASM and reserve formulas.
pub(crate) fn create_backstop(e: &Env) -> Address {
    let admin = Address::generate(e);
    let (blnd, _) = create_token(e, &admin);
    let (usdc, _) = create_token(e, &admin);
    let (xlm, _) = create_token(e, &admin);
    let blnd_usdc = create_comet_config(e, &blnd, &usdc);
    let blnd_xlm = create_comet_config(e, &blnd, &xlm);
    register_backstop(e, blnd, usdc, xlm, blnd_usdc, blnd_xlm)
}

pub(crate) fn create_backstop_with_real_comets(e: &Env) -> Address {
    let admin = Address::generate(e);
    let (blnd, _) = create_token(e, &admin);
    let (usdc, _) = create_token(e, &admin);
    let (xlm, _) = create_token(e, &admin);
    let (blnd_usdc, _) = create_comet_lp_pool(e, &admin, &blnd, &usdc);
    let (blnd_xlm, _) = create_comet_lp_pool(e, &admin, &blnd, &xlm);
    register_backstop(e, blnd, usdc, xlm, blnd_usdc, blnd_xlm)
}

fn register_backstop(
    e: &Env,
    blnd: Address,
    usdc: Address,
    xlm: Address,
    blnd_usdc: Address,
    blnd_xlm: Address,
) -> Address {
    let backstop = Address::generate(e);
    let pool_init_meta = PoolInitMeta {
        backstop: backstop.clone(),
        pool_hash: BytesN::<32>::from_array(e, &[0u8; 32]),
        blnd_id: blnd.clone(),
    };
    let pool_factory = e.register(MockPoolFactory {}, (pool_init_meta,));
    let emitter = e.register(MockEmitter, ());
    EmitterClient::new(e, &emitter).initialize(&blnd, &Address::generate(e), &blnd_usdc);
    e.register_at(
        &backstop,
        BackstopContract {},
        (
            blnd_usdc,
            blnd_xlm,
            emitter,
            blnd,
            usdc,
            xlm,
            pool_factory,
            Vec::<(Address, i128)>::new(e),
        ),
    );
    e.as_contract(&backstop, || {
        set_test_valuation_override(e, Some(false));
    });
    backstop
}

fn create_comet_config(e: &Env, blnd: &Address, pair: &Address) -> Address {
    e.register(MockCometConfig, (vec![e, blnd.clone(), pair.clone()],))
}

pub(crate) fn create_token<'a>(e: &Env, admin: &Address) -> (Address, MockTokenClient<'a>) {
    let contract_address = Address::generate(e);
    e.register_at(&contract_address, MockToken, ());
    let client = MockTokenClient::new(e, &contract_address);
    client.initialize(&admin, &7, &"unit".into_val(e), &"test".into_val(e));
    (contract_address, client)
}

pub(crate) fn create_blnd_token<'a>(
    e: &Env,
    backstop: &Address,
    admin: &Address,
) -> (Address, MockTokenClient<'a>) {
    let (contract_address, client) = create_token(e, admin);

    e.as_contract(backstop, || {
        storage::set_blnd_token(e, &contract_address);
    });
    (contract_address, client)
}

pub(crate) fn create_usdc_token<'a>(
    e: &Env,
    backstop: &Address,
    admin: &Address,
) -> (Address, MockTokenClient<'a>) {
    let (contract_address, client) = create_token(e, admin);

    e.as_contract(backstop, || {
        storage::set_usdc_token(e, &contract_address);
    });
    (contract_address, client)
}

pub(crate) fn create_backstop_token<'a>(
    e: &Env,
    backstop: &Address,
    admin: &Address,
) -> (Address, MockTokenClient<'a>) {
    let (contract_address, client) = create_token(e, admin);

    e.as_contract(backstop, || {
        storage::set_blnd_usdc_token(e, &contract_address);
    });
    (contract_address, client)
}

pub(crate) fn create_blnd_xlm_token<'a>(
    e: &Env,
    backstop: &Address,
    admin: &Address,
) -> (Address, MockTokenClient<'a>) {
    let (contract_address, client) = create_token(e, admin);

    e.as_contract(backstop, || {
        storage::set_blnd_xlm_token(e, &contract_address);
    });
    (contract_address, client)
}

// Not used to deploy pools in tests - filled with mock data
pub(crate) fn create_mock_pool_factory<'a>(
    e: &Env,
    backstop: &Address,
) -> (Address, MockPoolFactoryClient<'a>) {
    let contract_address = e.as_contract(backstop, || storage::get_pool_factory(e));
    (
        contract_address.clone(),
        MockPoolFactoryClient::new(e, &contract_address),
    )
}

pub(crate) fn create_emitter<'a>(
    e: &Env,
    backstop: &Address,
    backstop_token: &Address,
    blnd_token: &Address,
    emitter_last_distro: u64,
) -> (Address, EmitterClient<'a>) {
    let contract_address = e.register(MockEmitter, ());

    let prev_timestamp = e.ledger().timestamp();
    e.ledger().set(LedgerInfo {
        timestamp: emitter_last_distro,
        protocol_version: 27,
        sequence_number: 0,
        network_id: Default::default(),
        base_reserve: 10,
        min_temp_entry_ttl: 10,
        min_persistent_entry_ttl: 10,
        max_entry_ttl: 3110400,
    });
    e.as_contract(backstop, || {
        storage::set_emitter(e, &contract_address);
    });
    let client = EmitterClient::new(e, &contract_address);
    client.initialize(&blnd_token, &backstop, &backstop_token);
    e.ledger().set(LedgerInfo {
        timestamp: prev_timestamp,
        protocol_version: 27,
        sequence_number: 0,
        network_id: Default::default(),
        base_reserve: 10,
        min_temp_entry_ttl: 10,
        min_persistent_entry_ttl: 10,
        max_entry_ttl: 3110400,
    });
    (contract_address.clone(), client)
}

/// Deploy a test Comet LP pool of 80% BLND / 20% USDC and set it as the backstop token.
///
/// Initializes the pool with the following settings:
/// - Swap fee: 0.3%
/// - BLND: 1,000
/// - USDC: 25
/// - Shares: 100
pub(crate) fn create_comet_lp_pool<'a>(
    e: &Env,
    admin: &Address,
    blnd_token: &Address,
    usdc_token: &Address,
) -> (Address, CometClient<'a>) {
    let contract_address = Address::generate(e);
    e.register_at(&contract_address, COMET_WASM, ());
    let client = CometClient::new(e, &contract_address);

    let blnd_client = MockTokenClient::new(e, blnd_token);
    let usdc_client = MockTokenClient::new(e, usdc_token);
    blnd_client.mint(&admin, &1_000_0000000);
    usdc_client.mint(&admin, &25_0000000);

    client.init(
        admin,
        &vec![e, blnd_token.clone(), usdc_token.clone()],
        &vec![e, 0_8000000, 0_2000000],
        &vec![e, 1_000_0000000, 25_0000000],
        &0_0030000,
    );

    (contract_address, client)
}

pub(crate) fn create_mock_pool<'a>(e: &Env, _backstop: &Address) -> (Address, MockPoolClient<'a>) {
    let contract_address = Address::generate(e);
    (
        contract_address.clone(),
        MockPoolClient::new(e, &contract_address),
    )
}

/********** Comparison Helpers **********/

pub(crate) fn assert_eq_vec_q4w(actual: &Vec<Q4W>, expected: &Vec<Q4W>) {
    assert_eq!(actual.len(), expected.len());
    for index in 0..actual.len() {
        let actual_q4w = actual.get(index).unwrap_optimized();
        let expected_q4w = expected.get(index).unwrap_optimized();
        assert_eq!(actual_q4w.amount, expected_q4w.amount);
        assert_eq!(actual_q4w.exp, expected_q4w.exp);
    }
}
