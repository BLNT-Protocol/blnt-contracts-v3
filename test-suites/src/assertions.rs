use std::fmt::Debug;
use std::ops::{Add, Sub};

use crate::test_fixture::SCALAR_7;
use soroban_fixed_point_math::FixedPoint;
use soroban_sdk::{
    testutils::Events,
    xdr::{ContractEventBody, ScAddress, ScVal},
    Address, Env, TryFromVal, Val, Vec,
};

pub fn assert_approx_eq_abs<T>(a: T, b: T, delta: T)
where
    T: PartialOrd + Add<Output = T> + Sub<Output = T> + Copy + Debug,
{
    assert!(
        a > b - delta && a < b + delta,
        "assertion failed: `(left != right)` \
         (left: `{:?}`, right: `{:?}`, epsilon: `{:?}`)",
        a,
        b,
        delta
    );
}

/// Assert that `a` is approximately equal to `b` within a relative error of `delta`.
///
/// delta is a percentage such that 15% is 0_1500000
pub fn assert_approx_eq_rel(a: i128, b: i128, delta: i128) {
    let abs_delta = b.fixed_mul_floor(delta, SCALAR_7).unwrap();
    assert_approx_eq_abs(a, b, abs_delta);
}

/// Return an event in the tuple shape used by the SDK 22 test suite.
pub fn event_from_end(env: &Env, offset: usize) -> (Address, Vec<Val>, Val) {
    let events = legacy_contract_events(env);
    event_to_tuple(env, &events[events.len() - offset])
}

/// Snapshot all events in the tuple shape returned by the SDK 22 test API.
pub fn legacy_events(env: &Env) -> Vec<(Address, Vec<Val>, Val)> {
    let mut result = Vec::new(env);
    for event in legacy_contract_events(env) {
        result.push_back(event_to_tuple(env, &event));
    }
    result
}

fn legacy_contract_events(env: &Env) -> std::vec::Vec<soroban_sdk::xdr::ContractEvent> {
    let events = env.events().all();
    events
        .events()
        .iter()
        .filter(|event| event.contract_id.is_some())
        .cloned()
        .collect()
}

fn event_to_tuple(env: &Env, event: &soroban_sdk::xdr::ContractEvent) -> (Address, Vec<Val>, Val) {
    let contract_id = event.contract_id.clone().unwrap();
    let contract =
        Address::try_from_val(env, &ScVal::Address(ScAddress::Contract(contract_id))).unwrap();
    let ContractEventBody::V0(body) = &event.body;
    let mut topics = Vec::new(env);
    for topic in body.topics.iter() {
        topics.push_back(Val::try_from_val(env, topic).unwrap());
    }
    let data = Val::try_from_val(env, &body.data).unwrap();
    (contract, topics, data)
}
