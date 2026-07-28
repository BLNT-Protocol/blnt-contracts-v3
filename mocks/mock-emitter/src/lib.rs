#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error,
    token::StellarAssetClient, Address, Env, Vec,
};

const SCALAR_7: i128 = 10_000_000;
const MAX_DROP: i128 = 50_000_000 * SCALAR_7;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum EmitterError {
    AlreadyInitializedError = 3,
    BadDrop = 1101,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Backstop,
    BackstopToken,
    BlndToken,
    Dropped(Address),
    LastDistro(Address),
}

#[contract]
pub struct MockEmitter;

#[contractimpl]
impl MockEmitter {
    pub fn initialize(env: Env, blnd_token: Address, backstop: Address, backstop_token: Address) {
        if env.storage().instance().has(&DataKey::BlndToken) {
            panic_with_error!(&env, EmitterError::AlreadyInitializedError);
        }
        env.storage()
            .instance()
            .set(&DataKey::BlndToken, &blnd_token);
        env.storage().instance().set(&DataKey::Backstop, &backstop);
        env.storage()
            .instance()
            .set(&DataKey::BackstopToken, &backstop_token);
        set_last_distro(&env, &backstop, env.ledger().timestamp());
    }

    pub fn distribute(env: Env) -> i128 {
        let backstop = Self::get_backstop(env.clone());
        let last_distro = Self::get_last_distro(env.clone(), backstop.clone());
        let elapsed = env.ledger().timestamp() - last_distro;
        let amount = i128::from(elapsed) * SCALAR_7;
        set_last_distro(&env, &backstop, env.ledger().timestamp());
        let blnd: Address = env.storage().instance().get(&DataKey::BlndToken).unwrap();
        StellarAssetClient::new(&env, &blnd).mint(&backstop, &amount);
        amount
    }

    pub fn get_last_distro(env: Env, backstop_id: Address) -> u64 {
        env.storage()
            .persistent()
            .get(&DataKey::LastDistro(backstop_id))
            .unwrap()
    }

    pub fn get_backstop(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Backstop).unwrap()
    }

    pub fn drop(env: Env, list: Vec<(Address, i128)>) {
        let backstop = Self::get_backstop(env.clone());
        backstop.require_auth();
        if env
            .storage()
            .persistent()
            .get::<_, bool>(&DataKey::Dropped(backstop.clone()))
            .unwrap_or(false)
        {
            panic_with_error!(&env, EmitterError::BadDrop);
        }
        let mut total = 0_i128;
        for (_, amount) in list.iter() {
            total += amount;
        }
        if total > MAX_DROP {
            panic_with_error!(&env, EmitterError::BadDrop);
        }
        let blnd: Address = env.storage().instance().get(&DataKey::BlndToken).unwrap();
        let token = StellarAssetClient::new(&env, &blnd);
        for (recipient, amount) in list.iter() {
            token.mint(&recipient, &amount);
        }
        env.storage()
            .persistent()
            .set(&DataKey::Dropped(backstop), &true);
    }
}

fn set_last_distro(env: &Env, backstop: &Address, timestamp: u64) {
    env.storage()
        .persistent()
        .set(&DataKey::LastDistro(backstop.clone()), &timestamp);
}
