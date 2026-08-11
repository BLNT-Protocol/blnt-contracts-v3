use soroban_sdk::{contractclient, contracttype, Address, Env, Vec};

#[derive(Clone)]
#[contracttype(export = false)]
pub struct Swap {
    pub new_backstop: Address,
    pub new_backstop_token: Address,
    pub unlock_time: u64,
}

#[contractclient(name = "EmitterClient")]
#[allow(dead_code)]
pub trait Emitter {
    fn initialize(env: Env, blnd_token: Address, backstop: Address, backstop_token: Address);
    fn distribute(env: Env) -> i128;
    fn get_last_distro(env: Env, backstop_id: Address) -> u64;
    fn get_backstop(env: Env) -> Address;
    fn queue_swap_backstop(env: Env, new_backstop: Address, new_backstop_token: Address);
    fn get_queued_swap(env: Env) -> Option<Swap>;
    fn cancel_swap_backstop(env: Env);
    fn swap_backstop(env: Env);
    fn drop(env: Env, list: Vec<(Address, i128)>);
}
