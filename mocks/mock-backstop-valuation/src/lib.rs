#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, Address, Env};

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct AssetValuation {
    pub underlying_blnd: i128,
    pub usdc_value: i128,
    pub valid_until: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct BackstopValuationBinding {
    pub blnd: Address,
    pub blnd_usdc: Address,
    pub blnd_xlm: Address,
    pub usdc: Address,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Binding,
    Version,
}

#[contract]
pub struct MockBackstopValuation;

#[contractimpl]
impl MockBackstopValuation {
    pub fn __constructor(e: Env, binding: BackstopValuationBinding) {
        e.storage().instance().set(&DataKey::Binding, &binding);
        e.storage().instance().set(&DataKey::Version, &1u32);
    }

    pub fn version(e: Env) -> u32 {
        e.storage().instance().get(&DataKey::Version).unwrap()
    }

    pub fn binding(e: Env) -> BackstopValuationBinding {
        e.storage().instance().get(&DataKey::Binding).unwrap()
    }

    pub fn quote(_e: Env, _token: Address, amount: i128) -> AssetValuation {
        AssetValuation {
            underlying_blnd: amount,
            usdc_value: amount,
            valid_until: u64::MAX,
        }
    }

    pub fn set_version(e: Env, version: u32) {
        e.storage().instance().set(&DataKey::Version, &version);
    }
}
