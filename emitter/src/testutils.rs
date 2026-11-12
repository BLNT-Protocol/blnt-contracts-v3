#![cfg(test)]

use soroban_sdk::{testutils::Address as _, Address, Env};

use crate::EmitterContract;

pub(crate) fn create_emitter(e: &Env) -> Address {
    let initializer = Address::generate(e);
    e.register(EmitterContract, (initializer,))
}
