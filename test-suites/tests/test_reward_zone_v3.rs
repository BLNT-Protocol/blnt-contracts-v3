#![cfg(test)]

use backstop::BackstopTier;
use mock_pool_factory::MockPoolFactoryClient;
use soroban_sdk::{testutils::Address as _, vec, Address, String};
use test_suites::test_fixture::{TestFixture, TokenIndex, SCALAR_7};

fn exercise_reward_zone(wasm: bool) {
    let mut fixture = TestFixture::create(wasm);
    let e = fixture.env.clone();
    let depositor = fixture.users.first().unwrap().clone();

    fixture.create_pool(String::from_str(&e, "Reward zone one"), 0_1000000, 6, 0);
    let first = fixture.pools[0].pool.address.clone();
    let second = if wasm {
        fixture.create_pool(String::from_str(&e, "Reward zone two"), 0_1000000, 6, 0);
        fixture.pools[1].pool.address.clone()
    } else {
        // The native mock factory generates the same test address on each
        // deploy invocation, so register a distinct compatible mock directly.
        let pool = Address::generate(&e);
        MockPoolFactoryClient::new(&e, &fixture.pool_factory.address).set_mock_pool(&pool);
        pool
    };

    fixture.tokens[TokenIndex::BLND].mint(&depositor, &(1_000 * SCALAR_7));
    fixture.tokens[TokenIndex::USDC].mint(&depositor, &(30_000 * SCALAR_7));
    fixture.lp.join_pool(
        &(10 * SCALAR_7),
        &vec![&e, i128::MAX, i128::MAX],
        &depositor,
    );

    let lp_deposit = SCALAR_7;
    let usdc_deposit = fixture.backstop.activation_entry_threshold() - lp_deposit;
    for pool in [&first, &second] {
        fixture
            .backstop
            .deposit_blnd_usdc(&depositor, pool, &lp_deposit);
        fixture
            .backstop
            .deposit_usdc(&depositor, pool, &usdc_deposit);
    }

    fixture.backstop.add_reward(&first, &None);
    assert_eq!(fixture.backstop.reward_zone(), vec![&e, first.clone()]);
    assert!(fixture.backstop.try_add_reward(&second, &None).is_err());

    fixture.emitter.distribute();
    fixture.backstop.distribute();
    assert_eq!(
        fixture.backstop.reward_zone_checkpoint().unwrap().timestamp,
        e.ledger().timestamp()
    );
    assert!(!fixture.backstop.reward_zone().contains(&second));
    assert!(
        fixture
            .backstop
            .quote_pool_activation(&second, &false)
            .meets_threshold
    );
    assert!(
        fixture
            .backstop
            .pool_spot_blnd_emission_values(&second)
            .blnd_usdc
            > 0
    );
    fixture.backstop.add_reward(&second, &None);

    fixture
        .backstop
        .queue_blnd_usdc_withdrawal(&depositor, &second, &lp_deposit);
    fixture.jump(60 * 60 + 1);
    fixture.backstop.remove_reward(&second);
    assert!(!fixture.backstop.reward_zone().contains(&second));
    assert_eq!(
        fixture
            .backstop
            .tier_active_shares(&BackstopTier::BlndUsdc, &depositor, &second),
        0
    );
}

#[test]
fn native_reward_zone_uses_v3_membership_policy() {
    exercise_reward_zone(false);
}

#[test]
fn optimized_wasm_reward_zone_uses_v3_membership_policy() {
    exercise_reward_zone(true);
}
