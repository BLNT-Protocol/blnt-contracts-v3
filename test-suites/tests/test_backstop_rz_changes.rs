#![cfg(test)]
use pool::PoolClient;
use soroban_sdk::{
    testutils::{Address as _, BytesN as _},
    vec, Address, BytesN, String, Vec,
};
use test_suites::{
    create_fixture_with_data,
    test_fixture::{TokenIndex, SCALAR_7},
};

/// Test backstop RZ changes correctly handle emissions tracking
#[test]
fn test_backstop_rz_changes_handle_emissions() {
    let fixture = create_fixture_with_data(false);
    let bstop_token = &fixture.lp;
    let sam = Address::generate(&fixture.env);
    let frodo = &fixture.users[0];
    let pool_fixture = &fixture.pools[0];

    // Mint some backstop tokens
    // assumes Sam makes up 20% of the backstop after depositing (50k / 0.8 * 0.2 = 12.5k)
    //  -> mint 12.5k LP tokens to sam
    fixture.tokens[TokenIndex::BLND].mint(&sam, &(125_001_000_0000_0000_000_000 * SCALAR_7)); // 10 BLND per LP token
    fixture.tokens[TokenIndex::BLND].approve(&sam, &bstop_token.address, &i128::MAX, &99999);
    fixture.tokens[TokenIndex::USDC].mint(&sam, &(3_126_000_0000_0000_000_000 * SCALAR_7)); // 0.25 USDC per LP token
    fixture.tokens[TokenIndex::USDC].approve(&sam, &bstop_token.address, &i128::MAX, &99999);
    bstop_token.join_pool(
        &(12_500 * SCALAR_7),
        &vec![
            &fixture.env,
            125_001_000_0000_0000_000 * SCALAR_7,
            3_126_000_0000_0000_000 * SCALAR_7,
        ],
        &sam,
    );
    fixture.backstop.distribute();
    fixture.backstop.deposit(
        &backstop::BackstopTier::BlndUsdc,
        &sam,
        &pool_fixture.pool.address,
        &(12500 * SCALAR_7),
    );
    fixture.backstop.queue_withdrawal(
        &backstop::BackstopTier::BlndUsdc,
        frodo,
        &pool_fixture.pool.address,
        &(45000 * SCALAR_7),
    );

    fixture.jump(60 * 60 * 24 * 21);
    fixture.backstop.distribute();
    pool_fixture.pool.gulp_emissions();
    fixture.backstop.withdraw(
        &backstop::BackstopTier::BlndUsdc,
        frodo,
        &pool_fixture.pool.address,
        &(45000 * SCALAR_7),
        frodo,
    );

    // Move active value below the v3 maintenance threshold for the reward-zone
    // removal, then restore Sam's ordinary position at the same timestamp.
    // The fixture Comet is worth about $1.25 per LP share. Queue enough of
    // Sam's position to leave 7,500 active shares (about $9,375).
    let membership_reduction = 10_000 * SCALAR_7;
    fixture.backstop.queue_withdrawal(
        &backstop::BackstopTier::BlndUsdc,
        &sam,
        &pool_fixture.pool.address,
        &membership_reduction,
    );
    fixture.backstop.remove_reward(&pool_fixture.pool.address);
    fixture.backstop.dequeue_withdrawal(
        &backstop::BackstopTier::BlndUsdc,
        &sam,
        &pool_fixture.pool.address,
        &membership_reduction,
    );

    let result = pool_fixture.pool.try_gulp_emissions();
    assert!(result.is_err());

    // A reward-zone removal prevents new allocations, but the seven-day
    // stream already started by the pool remains claimable through expiry.
    fixture.jump(60 * 60 * 24 * 3);
    let accrued = fixture.backstop.claimable(
        &backstop::BackstopTier::BlndUsdc,
        &sam,
        &vec![&fixture.env, pool_fixture.pool.address.clone()],
    );
    assert!(accrued > 0);
    assert!(
        fixture.backstop.claim(
            &backstop::BackstopTier::BlndUsdc,
            &sam,
            &vec![&fixture.env, pool_fixture.pool.address.clone()],
            &0,
        ) > 0
    );

    fixture.jump(60 * 60 * 24 * 4);
    assert!(
        fixture.backstop.claim(
            &backstop::BackstopTier::BlndUsdc,
            &sam,
            &vec![&fixture.env, pool_fixture.pool.address.clone()],
            &0,
        ) > 0
    );

    fixture.backstop.deposit(
        &backstop::BackstopTier::BlndUsdc,
        frodo,
        &pool_fixture.pool.address,
        &(50000 * SCALAR_7),
    );

    fixture
        .backstop
        .add_reward(&pool_fixture.pool.address, &None);

    fixture.backstop.distribute();

    let result = pool_fixture.pool.gulp_emissions();

    // Emissions are distributed to the pool because the reward zone was empty when the backstop was added
    assert_eq!(result, 1814400000000); // (60 * 60 * 24 * 7) * 0.3
}

#[test]
fn test_backstop_full_rz_under_limits() {
    let fixture = create_fixture_with_data(true);
    let bstop_token = &fixture.lp;
    let sam = Address::generate(&fixture.env);
    let pool_fixture = &fixture.pools[0];

    // Mint some backstop tokens
    let per_pool_lp_deposit = 30_000 * SCALAR_7;
    fixture.tokens[TokenIndex::BLND].mint(&sam, &(125_001_000_0000_0000_000_000 * SCALAR_7)); // 10 BLND per LP token
    fixture.tokens[TokenIndex::BLND].approve(&sam, &bstop_token.address, &i128::MAX, &99999);
    fixture.tokens[TokenIndex::USDC].mint(&sam, &(3_126_000_0000_0000_000_000 * SCALAR_7)); // 0.25 USDC per LP token
    fixture.tokens[TokenIndex::USDC].approve(&sam, &bstop_token.address, &i128::MAX, &99999);
    bstop_token.join_pool(
        &(per_pool_lp_deposit * 60),
        &vec![
            &fixture.env,
            125_001_000_0000_0000_000 * SCALAR_7,
            3_126_000_0000_0000_000 * SCALAR_7,
        ],
        &sam,
    );

    // 1 Pool already in rz. Create 29 new pools.
    // They don't need reserves as we're not going to use them.
    fixture.backstop.distribute();
    let mut pools: Vec<Address> = vec![&fixture.env, pool_fixture.pool.address.clone()];
    for _ in 0..29 {
        let pool_address = fixture.pool_factory.deploy(
            &sam,
            &String::from_str(&fixture.env, "Teapot"),
            &BytesN::<32>::random(&fixture.env),
            &fixture.oracle.address,
            &0,
            &6,
            &0,
        );
        fixture.backstop.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &sam,
            &pool_address,
            &per_pool_lp_deposit,
        );
        fixture.backstop.add_reward(&pool_address, &None);
        pools.push_back(pool_address);
    }

    // check rz length
    let rz = fixture.backstop.reward_zone();
    assert_eq!(rz.len(), 30);

    // Run distribute w/ 30 pools
    fixture.jump_with_sequence(60 * 60 * 24 * 5);
    fixture.backstop.distribute();
    let dist_resources = fixture.env.cost_estimate().resources();
    assert!(dist_resources.instructions < 100000000);
    assert!(dist_resources.mem_bytes < 41943040 / 2);
    assert!(
        dist_resources.disk_read_entries
            + dist_resources.memory_read_entries
            + dist_resources.write_entries
            < 100
    );
    assert!(dist_resources.write_entries < 50);
    assert!(dist_resources.disk_read_bytes < 200000 / 2);
    assert!(dist_resources.write_bytes < 132096 / 2);

    // The configured fixture pool can reserve its tranche. The 29 pools that
    // intentionally have no reserves or emission configuration must reject
    // before creating an unreachable candidate reservation.
    assert!(PoolClient::new(&fixture.env, &pools.get_unchecked(0)).gulp_emissions() > 0);
    for index in 1..pools.len() {
        assert!(PoolClient::new(&fixture.env, &pools.get_unchecked(index))
            .try_gulp_emissions()
            .is_err());
    }
}
