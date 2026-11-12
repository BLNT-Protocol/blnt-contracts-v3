use soroban_sdk::{contractclient, Address, Env};

#[contractclient(name = "AccessControllerClient")]
#[allow(dead_code)]
pub trait AccessController {
    fn permissions(e: Env, pool: Address, user: Address) -> u32;
}
