#![cfg(test)]
#![allow(clippy::zero_prefixed_literal)]

use backstop::BackstopTier;
use mock_pool_factory::{
    BackstopAsset as MockBackstopAsset, BackstopTierConfig as MockBackstopTierConfig,
    MockPoolFactoryClient,
};
use soroban_sdk::{testutils::Address as _, vec, Address};
use test_suites::{
    create_fixture_with_data,
    test_fixture::{TokenIndex, SCALAR_7},
};

/// A backstop with no BLNT-bearing tier is immune to the anchor manipulation.
///
/// `build_pool_valuation` reads Comet lazily -- `needs_anchor` / `needs_target`
/// are set only by `BlntUsdc`, `BlntXlm` and `Xlm`
/// (`backstop/src/backstop/pool.rs:339-342`), and the reads are behind
/// `needs_anchor.then(|| read_comet(..))` (`:394`). A pool configured with only
/// `BackstopAsset::Usdc` takes the `unit_pool_tier_valuation` pass-through
/// (`:498`), so its `active_value` has no Comet input to manipulate.
///
/// This bounds BUG1 §3.3/§3.5 and the manipulation route into BUG2: they require
/// the victim to hold a BLNT-bearing tier.
///
/// It does NOT make such a pool immune to everything -- see the final assertion.
#[test]
fn usdc_only_backstop_is_immune_to_anchor_manipulation() {
    let fixture = create_fixture_with_data(false);
    let e = fixture.env.clone();
    let blnt = &fixture.tokens[TokenIndex::BLNT];
    let usdc = &fixture.tokens[TokenIndex::USDC];
    let depositor = fixture.users[0].clone();
    let attacker = fixture.users[0].clone();

    // The stock fixture pool carries BlntXlm + BlntUsdc + Usdc tiers: the control.
    let mixed = fixture.pools[0].pool.address.clone();

    // A second pool configured with a single USDC tier and nothing else.
    let usdc_only = Address::generate(&e);
    let factory = MockPoolFactoryClient::new(&e, &fixture.pool_factory.address);
    factory.set_pool_config(
        &usdc_only,
        &vec![
            &e,
            MockBackstopTierConfig {
                asset: MockBackstopAsset::Usdc,
                take_rate_weight: 1,
            },
        ],
    );
    usdc.mint(&depositor, &(60_000 * SCALAR_7));
    fixture.backstop.deposit(
        &BackstopTier::FirstLoss,
        &depositor,
        &usdc_only,
        &(50_000 * SCALAR_7),
    );

    let usdc_only_before = fixture.backstop.pool_data(&usdc_only).active_value;
    let mixed_before = fixture.backstop.pool_data(&mixed).active_value;
    let price_before = fixture.backstop.blnt_price();
    std::println!("usdc-only active_value before = {}", usdc_only_before);
    std::println!("mixed     active_value before = {}", mixed_before);
    assert!(usdc_only_before > 0);

    // Same anchor deflation used against the threshold in BUG1 §3.5.
    blnt.mint(&attacker, &(2_000_000_000 * SCALAR_7));
    for _ in 0..2 {
        let reserve = fixture.lp.get_balance(&blnt.address);
        fixture.lp.swap_exact_amount_in(
            &blnt.address,
            &(reserve / 4),
            &usdc.address,
            &0,
            &i128::MAX,
            &attacker,
        );
    }

    let usdc_only_after = fixture.backstop.pool_data(&usdc_only).active_value;
    let mixed_after = fixture.backstop.pool_data(&mixed).active_value;
    let price_after = fixture.backstop.blnt_price();
    std::println!("usdc-only active_value after  = {}", usdc_only_after);
    std::println!(
        "mixed     active_value after  = {} ({}% of before)",
        mixed_after,
        mixed_after * 100 / mixed_before
    );
    std::println!(
        "blnt_price {} -> {} ({}%)",
        price_before,
        price_after,
        price_after * 100 / price_before
    );

    // The swap really did move the anchor: the control pool's value collapsed.
    assert!(
        mixed_after < mixed_before / 2,
        "control pool must be materially devalued, else the test proves nothing"
    );
    assert!(price_after < price_before, "blnt_price must have moved");

    // The finding: the USDC-only pool did not move at all. Not "moved a little" --
    // exactly equal, because no Comet read enters its valuation.
    assert_eq!(
        usdc_only_after, usdc_only_before,
        "a backstop with no BLNT-bearing tier has no anchor exposure"
    );

    // Bound the claim. `blnt_price` is a global read with no pool input, so the
    // protocol-fee auction bid -- which is denominated in BLNT for every pool
    // regardless of backstop composition -- is still quoted off the manipulated
    // anchor. A USDC-only backstop closes the threshold and tier-value legs, not
    // this one.
    assert!(
        price_after != price_before,
        "protocol-fee bids stay exposed: blnt_price is pool-independent"
    );
}
