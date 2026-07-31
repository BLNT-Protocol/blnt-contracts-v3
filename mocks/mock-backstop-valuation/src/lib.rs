#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, Address, Env,
};

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
    QuoteFailure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[contracterror]
#[repr(u32)]
enum MockBackstopValuationError {
    QuoteFailure = 1,
}

#[contract]
pub struct MockBackstopValuation;

#[contractimpl]
impl MockBackstopValuation {
    pub fn __constructor(e: Env, binding: BackstopValuationBinding) {
        e.storage().instance().set(&DataKey::Binding, &binding);
    }

    pub fn binding(e: Env) -> BackstopValuationBinding {
        e.storage().instance().get(&DataKey::Binding).unwrap()
    }

    pub fn quote(e: Env, _token: Address, amount: i128) -> AssetValuation {
        if e.storage()
            .instance()
            .get(&DataKey::QuoteFailure)
            .unwrap_or(false)
        {
            panic_with_error!(&e, MockBackstopValuationError::QuoteFailure);
        }
        AssetValuation {
            underlying_blnd: amount,
            usdc_value: amount,
            valid_until: u64::MAX,
        }
    }

    /// Test-only failure injection for fail-closed valuation paths.
    pub fn set_quote_failure(e: Env, fail: bool) {
        e.storage().instance().set(&DataKey::QuoteFailure, &fail);
    }
}
