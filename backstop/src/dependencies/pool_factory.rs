use soroban_sdk::{contractclient, Address, Env};

#[contractclient(name = "PoolFactoryClient")]
#[allow(dead_code)]
pub trait PoolFactory {
    fn backstop(e: Env) -> Address;
    fn is_pool(e: Env, pool: Address) -> bool;
}
