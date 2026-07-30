use crate::{
    storage::{self, PoolInitMeta},
    PoolFactoryError,
};
use soroban_sdk::{
    contract, contractevent, contractimpl, panic_with_error, testutils::Address as _, Address,
    BytesN, Env, String,
};

use mock_pool::{MockPool, MockPoolClient};
use pool::PoolContract;

#[contractevent(topics = ["deploy"], data_format = "single-value")]
struct MockPoolFactoryDeployEvent {
    pool_address: Address,
}

#[contract]
pub struct MockPoolFactory;

pub trait MockPoolFactoryTrait {
    /// Fetch the immutable backstop used by pools from this factory.
    fn backstop(e: Env) -> Address;

    /// Fetch the immutable pool WASM hash used by this factory.
    fn pool_wasm_hash(e: Env) -> BytesN<32>;

    /// Deploys and initializes a lending pool
    ///
    /// # Arguments
    /// * `admin` - The admin address for the pool
    /// * `name` - The name of the pool
    /// * `oracle` - The oracle address for the pool
    /// * `backstop_take_rate` - The backstop take rate for the pool (7 decimals)
    /// * `max_positions` - The maximum user positions supported by the pool
    /// * `min_collateral` - The minimum collateral required for a position
    fn deploy(
        e: Env,
        admin: Address,
        name: String,
        salt: BytesN<32>,
        oracle: Address,
        backstop_take_rate: u32,
        max_positions: u32,
        min_collateral: i128,
    ) -> Address;

    /// Checks if contract address was deployed by the factory
    ///
    /// Returns true if pool was deployed by factory and false otherwise
    ///
    /// # Arguments
    /// * 'pool_address' - The contract address to be checked
    fn is_pool(e: Env, pool_address: Address) -> bool;

    /// Mock Only: Set a pool_address as having been deployed by the pool factory
    ///
    /// ### Arguments
    /// * `pool_address` - The pool address to set
    fn set_pool(e: Env, pool_address: Address);

    /// Mock only: register a compatible mock pool and mark it deployed.
    fn set_mock_pool(e: Env, pool_address: Address);
}

#[contractimpl]
impl MockPoolFactory {
    /// Construct the pool factory contract
    ///
    /// ### Arguments
    /// * `pool_init_meta` - The pool initialization metadata    
    pub fn __constructor(e: Env, pool_init_meta: PoolInitMeta) {
        storage::set_pool_init_meta(&e, &pool_init_meta);
    }
}

#[contractimpl]
impl MockPoolFactoryTrait for MockPoolFactory {
    fn backstop(e: Env) -> Address {
        storage::get_pool_init_meta(&e).backstop
    }

    fn pool_wasm_hash(e: Env) -> BytesN<32> {
        storage::get_pool_init_meta(&e).pool_hash
    }

    fn deploy(
        e: Env,
        admin: Address,
        name: String,
        _salt: BytesN<32>,
        oracle: Address,
        backstop_take_rate: u32,
        max_positions: u32,
        min_collateral: i128,
    ) -> Address {
        storage::extend_instance(&e);
        admin.require_auth();
        let pool_init_meta = storage::get_pool_init_meta(&e);

        // verify backstop take rate is within [0,1) with 9 decimals
        if backstop_take_rate >= 1_0000000 {
            panic_with_error!(&e, PoolFactoryError::InvalidPoolInitArgs);
        }

        let pool_address = Address::generate(&e);
        e.register_at(
            &pool_address,
            PoolContract {},
            (
                admin,
                name,
                oracle,
                backstop_take_rate,
                max_positions,
                min_collateral,
                pool_init_meta.backstop,
                pool_init_meta.blnd_id,
            ),
        );

        storage::set_deployed(&e, &pool_address);

        MockPoolFactoryDeployEvent {
            pool_address: pool_address.clone(),
        }
        .publish(&e);
        pool_address
    }

    fn is_pool(e: Env, pool_address: Address) -> bool {
        storage::is_deployed(&e, &pool_address)
    }

    fn set_pool(e: Env, pool_address: Address) {
        storage::set_deployed(&e, &pool_address);
    }

    fn set_mock_pool(e: Env, pool_address: Address) {
        let backstop = storage::get_pool_init_meta(&e).backstop;
        e.register_at(&pool_address, MockPool, ());
        MockPoolClient::new(&e, &pool_address).set_backstop(&backstop);
        storage::set_deployed(&e, &pool_address);
    }
}
