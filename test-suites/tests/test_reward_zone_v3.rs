#![cfg(test)]

use backstop::BackstopTier;
use mock_pool_factory::MockPoolFactoryClient;
use soroban_sdk::{testutils::Address as _, vec, Address, String};
use test_suites::test_fixture::{TestFixture, TokenIndex, SCALAR_7};

const ACTIVATION_THRESHOLD_USDC: i128 = 12_500 * SCALAR_7;

fn exercise_reward_zone(wasm: bool) {
    let mut fixture = TestFixture::create(wasm);
    let e = fixture.env.clone();
    let depositor = fixture.users.first().unwrap().clone();

    fixture.create_pool(String::from_str(&e, "Reward zone one"), 0_1000000, 6, 0);
    let first = fixture.pools[0].pool.address.clone();
    let (second, third) = if wasm {
        fixture.create_pool(String::from_str(&e, "Reward zone two"), 0_1000000, 6, 0);
        fixture.create_pool(String::from_str(&e, "Reward zone three"), 0_1000000, 6, 0);
        (
            fixture.pools[1].pool.address.clone(),
            fixture.pools[2].pool.address.clone(),
        )
    } else {
        // The native mock factory generates the same test address on each
        // deploy invocation, so register a distinct compatible mock directly.
        let second = Address::generate(&e);
        let third = Address::generate(&e);
        let factory = MockPoolFactoryClient::new(&e, &fixture.pool_factory.address);
        factory.set_pool(&second);
        factory.set_pool(&third);
        (second, third)
    };

    fixture.tokens[TokenIndex::BLND].mint(&depositor, &(1_000 * SCALAR_7));
    fixture.tokens[TokenIndex::USDC].mint(&depositor, &(50_000 * SCALAR_7));
    fixture.lp.join_pool(
        &(10 * SCALAR_7),
        &vec![&e, i128::MAX, i128::MAX],
        &depositor,
    );

    let lp_deposit = SCALAR_7;
    let usdc_deposit = ACTIVATION_THRESHOLD_USDC - lp_deposit;
    for pool in [&first, &second, &third] {
        fixture.backstop.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &depositor,
            pool,
            &lp_deposit,
        );
        fixture.backstop.deposit(
            &backstop::BackstopTier::Usdc,
            &depositor,
            pool,
            &usdc_deposit,
        );
    }

    fixture.backstop.add_reward(&first, &None);
    fixture.backstop.distribute();
    fixture.jump(6);
    let first_tokens_before = fixture.backstop.pool_data(&first).blnd_usdc.tokens;
    fixture
        .backstop
        .deposit(&BackstopTier::BlndUsdc, &depositor, &first, &lp_deposit);
    assert_eq!(
        fixture.backstop.pool_data(&first).blnd_usdc.tokens,
        first_tokens_before + lp_deposit
    );
    fixture.backstop.add_reward(&second, &None);
    assert_eq!(fixture.backstop.reward_zone().len(), 2);
    assert!(fixture.backstop.reward_zone().contains(&first));
    assert!(fixture.backstop.reward_zone().contains(&second));

    let third_data = fixture.backstop.pool_data(&third);
    assert!(third_data.active_value >= ACTIVATION_THRESHOLD_USDC);
    assert!(third_data.blnd_usdc.tokens > 0);
    fixture.jump(60 * 60 + 1);
    assert!(fixture.backstop.try_add_reward(&third, &None).is_err());
    fixture.backstop.distribute();
    fixture.backstop.add_reward(&third, &None);

    fixture.backstop.queue_withdrawal(
        &backstop::BackstopTier::BlndUsdc,
        &depositor,
        &second,
        &lp_deposit,
    );
    fixture.backstop.remove_reward(&second);
    assert!(!fixture.backstop.reward_zone().contains(&second));
    assert_eq!(
        fixture
            .backstop
            .user_balance(&BackstopTier::BlndUsdc, &second, &depositor)
            .shares,
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
