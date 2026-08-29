#![cfg(test)]

use soroban_sdk::{testutils::Address as _, Address, Env};

use crate::EmitterContract;

pub(crate) fn create_emitter(e: &Env) -> Address {
    let legacy_admin = Address::generate(e);
    let legacy_blnd = e.register_stellar_asset_contract_v2(legacy_admin).address();
    create_emitter_with_legacy(e, &legacy_blnd)
}

pub(crate) fn create_emitter_with_legacy(e: &Env, legacy_blnd: &Address) -> Address {
    let initializer = Address::generate(e);
    e.register(EmitterContract, (legacy_blnd, initializer))
}
