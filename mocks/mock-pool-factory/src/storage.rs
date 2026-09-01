use soroban_sdk::{contracttype, unwrap::UnwrapOptimized, Address, BytesN, Env, Vec};

/********** Ledger Thresholds **********/

const ONE_DAY_LEDGERS: u32 = 17280; // assumes 5s a ledger

const LEDGER_THRESHOLD_INSTANCE: u32 = ONE_DAY_LEDGERS * 30; // ~ 30 days
const LEDGER_BUMP_INSTANCE: u32 = LEDGER_THRESHOLD_INSTANCE + ONE_DAY_LEDGERS; // ~ 31 days

const LEDGER_THRESHOLD_SHARED: u32 = ONE_DAY_LEDGERS * 45; // ~ 45 days
const LEDGER_BUMP_SHARED: u32 = LEDGER_THRESHOLD_SHARED + ONE_DAY_LEDGERS; // ~ 46 days

#[derive(Clone)]
#[contracttype]
pub enum PoolFactoryDataKey {
    Contracts(Address),
    BackstopConfig(Address),
    DefaultBackstopConfig,
    PoolInitMeta,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[contracttype]
pub enum BackstopAsset {
    BlntXlm,
    BlntUsdc,
    Usdc,
    Xlm,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct BackstopTierConfig {
    pub asset: BackstopAsset,
    pub take_rate_weight: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct PoolBackstopConfig {
    pub access_controller: Option<Address>,
    pub tiers: Vec<BackstopTierConfig>,
}

#[derive(Clone)]
#[contracttype]
pub struct PoolInitMeta {
    pub pool_hash: BytesN<32>,
    pub backstop: Address,
    pub blnt_id: Address,
}

/// Bump the instance rent for the contract
pub fn extend_instance(e: &Env) {
    e.storage()
        .instance()
        .extend_ttl(LEDGER_THRESHOLD_INSTANCE, LEDGER_BUMP_INSTANCE);
}

/// Fetch the pool initialization metadata
pub fn get_pool_init_meta(e: &Env) -> PoolInitMeta {
    e.storage()
        .instance()
        .get::<PoolFactoryDataKey, PoolInitMeta>(&PoolFactoryDataKey::PoolInitMeta)
        .unwrap_optimized()
}

/// Set the pool initialization metadata
///
/// ### Arguments
/// * `pool_init_meta` - The metadata to initialize pools
pub fn set_pool_init_meta(e: &Env, pool_init_meta: &PoolInitMeta) {
    e.storage()
        .instance()
        .set::<PoolFactoryDataKey, PoolInitMeta>(&PoolFactoryDataKey::PoolInitMeta, pool_init_meta)
}

/// Check if a given contract_id was deployed by the factory
///
/// ### Arguments
/// * `contract_id` - The contract_id to check
pub fn is_deployed(e: &Env, contract_id: &Address) -> bool {
    let key = PoolFactoryDataKey::Contracts(contract_id.clone());
    if let Some(result) = e
        .storage()
        .persistent()
        .get::<PoolFactoryDataKey, bool>(&key)
    {
        e.storage()
            .persistent()
            .extend_ttl(&key, LEDGER_THRESHOLD_SHARED, LEDGER_BUMP_SHARED);
        result
    } else {
        false
    }
}

/// Set a contract_id as having been deployed by the factory
///
/// ### Arguments
/// * `contract_id` - The contract_id that was deployed by the factory
pub fn set_deployed(
    e: &Env,
    contract_id: &Address,
    backstop_config: &Vec<BackstopTierConfig>,
    access_controller: &Option<Address>,
) {
    let key = PoolFactoryDataKey::Contracts(contract_id.clone());
    e.storage()
        .persistent()
        .set::<PoolFactoryDataKey, bool>(&key, &true);
    e.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_THRESHOLD_SHARED, LEDGER_BUMP_SHARED);
    let config_key = PoolFactoryDataKey::BackstopConfig(contract_id.clone());
    e.storage().persistent().set(
        &config_key,
        &PoolBackstopConfig {
            access_controller: access_controller.clone(),
            tiers: backstop_config.clone(),
        },
    );
    e.storage()
        .persistent()
        .extend_ttl(&config_key, LEDGER_THRESHOLD_SHARED, LEDGER_BUMP_SHARED);
}

pub fn get_backstop_config(e: &Env, contract_id: &Address) -> PoolBackstopConfig {
    let key = PoolFactoryDataKey::BackstopConfig(contract_id.clone());
    e.storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| PoolBackstopConfig {
            access_controller: None,
            tiers: Vec::new(e),
        })
}

pub fn set_access_controller(e: &Env, contract_id: &Address, access_controller: &Option<Address>) {
    let key = PoolFactoryDataKey::BackstopConfig(contract_id.clone());
    let mut config = get_backstop_config(e, contract_id);
    config.access_controller = access_controller.clone();
    e.storage().persistent().set(&key, &config);
}

pub fn set_default_backstop_config(e: &Env, config: &Vec<BackstopTierConfig>) {
    e.storage()
        .instance()
        .set(&PoolFactoryDataKey::DefaultBackstopConfig, config);
}

pub fn get_default_backstop_config(e: &Env) -> Vec<BackstopTierConfig> {
    e.storage()
        .instance()
        .get(&PoolFactoryDataKey::DefaultBackstopConfig)
        .unwrap_or_else(|| Vec::new(e))
}
