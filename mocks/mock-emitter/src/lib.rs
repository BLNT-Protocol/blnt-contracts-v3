#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error,
    token::{StellarAssetClient, TokenClient},
    Address, Env, Vec,
};

const SCALAR_7: i128 = 10_000_000;
const MAX_INITIAL_DROP: i128 = 150_000_000 * SCALAR_7;
const MAX_MIGRATION_DROP: i128 = 50_000_000 * SCALAR_7;
const QUEUE_SECONDS: u64 = 31 * 24 * 60 * 60;
const SWAP_GRACE_SECONDS: u64 = 7 * 24 * 60 * 60;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum EmitterError {
    AlreadyInitializedError = 3,
    InsufficientBackstopSize = 1100,
    BadDrop = 1101,
    SwapNotQueued = 1102,
    SwapAlreadyExists = 1103,
    SwapNotUnlocked = 1104,
    SwapCannotBeCanceled = 1105,
    OverflowError = 1112,
    SwapExpired = 1113,
}

#[derive(Clone)]
#[contracttype]
pub struct Swap {
    pub new_backstop: Address,
    pub new_backstop_token: Address,
    pub unlock_time: u64,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Backstop,
    InitialBackstop,
    BackstopToken,
    BlntToken,
    Dropped(Address),
    LastDistro(Address),
    Swap,
}

#[contract]
pub struct MockEmitter;

#[contractimpl]
impl MockEmitter {
    pub fn initialize(env: Env, blnt_token: Address, backstop: Address, backstop_token: Address) {
        if env.storage().instance().has(&DataKey::BlntToken) {
            panic_with_error!(&env, EmitterError::AlreadyInitializedError);
        }
        env.storage()
            .instance()
            .set(&DataKey::BlntToken, &blnt_token);
        env.storage().instance().set(&DataKey::Backstop, &backstop);
        env.storage()
            .instance()
            .set(&DataKey::InitialBackstop, &backstop);
        env.storage()
            .instance()
            .set(&DataKey::BackstopToken, &backstop_token);
        set_last_distro(&env, &backstop, env.ledger().timestamp());
    }

    pub fn distribute(env: Env) -> i128 {
        let backstop = Self::get_backstop(env.clone());
        backstop.require_auth();
        distribute_to(&env, &backstop)
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

    pub fn queue_swap_backstop(env: Env, new_backstop: Address, new_backstop_token: Address) {
        if env.storage().persistent().has(&DataKey::Swap) {
            panic_with_error!(&env, EmitterError::SwapAlreadyExists);
        }
        require_larger_backstop(&env, &new_backstop);
        let unlock_time = env
            .ledger()
            .timestamp()
            .checked_add(QUEUE_SECONDS)
            .unwrap_or_else(|| panic_with_error!(&env, EmitterError::OverflowError));
        let swap = Swap {
            new_backstop,
            new_backstop_token,
            unlock_time,
        };
        swap_deadline(&env, &swap);
        env.storage().persistent().set(&DataKey::Swap, &swap);
    }

    pub fn get_queued_swap(env: Env) -> Option<Swap> {
        env.storage().persistent().get(&DataKey::Swap)
    }

    pub fn cancel_swap_backstop(env: Env) {
        let swap: Swap = env
            .storage()
            .persistent()
            .get(&DataKey::Swap)
            .unwrap_or_else(|| panic_with_error!(&env, EmitterError::SwapNotQueued));
        let expired = env.ledger().timestamp() > swap_deadline(&env, &swap);
        if !expired && is_larger_backstop(&env, &swap.new_backstop) {
            panic_with_error!(&env, EmitterError::SwapCannotBeCanceled);
        }
        env.storage().persistent().remove(&DataKey::Swap);
    }

    pub fn swap_backstop(env: Env) {
        let swap: Swap = env
            .storage()
            .persistent()
            .get(&DataKey::Swap)
            .unwrap_or_else(|| panic_with_error!(&env, EmitterError::SwapNotQueued));
        if env.ledger().timestamp() < swap.unlock_time {
            panic_with_error!(&env, EmitterError::SwapNotUnlocked);
        }
        if env.ledger().timestamp() > swap_deadline(&env, &swap) {
            panic_with_error!(&env, EmitterError::SwapExpired);
        }
        require_larger_backstop(&env, &swap.new_backstop);

        let incumbent = Self::get_backstop(env.clone());
        distribute_to(&env, &incumbent);
        env.storage().persistent().remove(&DataKey::Swap);
        env.storage()
            .instance()
            .set(&DataKey::Backstop, &swap.new_backstop);
        env.storage()
            .instance()
            .set(&DataKey::BackstopToken, &swap.new_backstop_token);
        set_last_distro(&env, &swap.new_backstop, env.ledger().timestamp());
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
        let initial_backstop: Address = env
            .storage()
            .instance()
            .get(&DataKey::InitialBackstop)
            .unwrap();
        let max_drop = if backstop == initial_backstop {
            MAX_INITIAL_DROP
        } else {
            MAX_MIGRATION_DROP
        };
        for (_, amount) in list.iter() {
            if amount.is_negative() {
                panic_with_error!(&env, EmitterError::BadDrop);
            }
            total = total
                .checked_add(amount)
                .unwrap_or_else(|| panic_with_error!(&env, EmitterError::OverflowError));
            if total > max_drop {
                panic_with_error!(&env, EmitterError::BadDrop);
            }
        }
        let blnt: Address = env.storage().instance().get(&DataKey::BlntToken).unwrap();
        let token = StellarAssetClient::new(&env, &blnt);
        for (recipient, amount) in list.iter() {
            token.mint(&recipient, &amount);
        }
        env.storage()
            .persistent()
            .set(&DataKey::Dropped(backstop), &true);
    }
}

fn is_larger_backstop(env: &Env, candidate: &Address) -> bool {
    let incumbent: Address = env.storage().instance().get(&DataKey::Backstop).unwrap();
    let token: Address = env
        .storage()
        .instance()
        .get(&DataKey::BackstopToken)
        .unwrap();
    let client = TokenClient::new(env, &token);
    client.balance(candidate) > client.balance(&incumbent)
}

fn require_larger_backstop(env: &Env, candidate: &Address) {
    if !is_larger_backstop(env, candidate) {
        panic_with_error!(env, EmitterError::InsufficientBackstopSize);
    }
}

fn swap_deadline(env: &Env, swap: &Swap) -> u64 {
    swap.unlock_time
        .checked_add(SWAP_GRACE_SECONDS)
        .unwrap_or_else(|| panic_with_error!(env, EmitterError::OverflowError))
}

fn distribute_to(env: &Env, backstop: &Address) -> i128 {
    let last_distro = MockEmitter::get_last_distro(env.clone(), backstop.clone());
    let elapsed = env.ledger().timestamp() - last_distro;
    let amount = i128::from(elapsed) * SCALAR_7;
    set_last_distro(env, backstop, env.ledger().timestamp());
    let blnt: Address = env.storage().instance().get(&DataKey::BlntToken).unwrap();
    StellarAssetClient::new(env, &blnt).mint(backstop, &amount);
    amount
}

fn set_last_distro(env: &Env, backstop: &Address, timestamp: u64) {
    env.storage()
        .persistent()
        .set(&DataKey::LastDistro(backstop.clone()), &timestamp);
}
