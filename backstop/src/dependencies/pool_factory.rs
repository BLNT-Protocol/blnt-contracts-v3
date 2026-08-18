use soroban_sdk::{contractclient, contracttype, Address, Env, Vec};

/// One of the four canonical assets accepted by the v3 backstop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[contracttype(export = false)]
pub enum BackstopAsset {
    BlndXlm,
    BlndUsdc,
    Usdc,
    Xlm,
}

/// One pool-factory-provided immutable loss-waterfall position.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype(export = false)]
pub struct BackstopTierConfig {
    pub asset: BackstopAsset,
    pub take_rate_weight: u32,
}

#[contractclient(name = "PoolFactoryClient")]
#[allow(dead_code)]
pub trait PoolFactory {
    fn is_pool(e: Env, pool: Address) -> bool;
    fn backstop_config(e: Env, pool: Address) -> Vec<BackstopTierConfig>;
}
