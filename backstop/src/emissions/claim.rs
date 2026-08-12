use sep_41_token::TokenClient;
use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    panic_with_error, vec, Address, Env, IntoVal, Map, Symbol, Val, Vec,
};

#[cfg(test)]
use crate::storage::UserEmissionData;
use crate::{
    backstop::{credit_tier_shares, require_registered_pool, tier_token, BackstopTier},
    dependencies::CometClient,
    errors::BackstopError,
    migration, storage,
};

use super::{
    distributor,
    manager::{
        checked_add, finish_pool_weight_change, get_ongoing_emission_state,
        prepare_pool_weight_change, set_ongoing_emission_state,
    },
    policy::proportional_floor,
};

pub(crate) struct ClaimResult {
    pub lp_amount: i128,
    pub allocations: Vec<(Address, i128, i128, i128)>,
}

pub fn execute_claim(
    e: &Env,
    tier: BackstopTier,
    from: &Address,
    pool_addresses: &Vec<Address>,
    min_lp_tokens_out: i128,
) -> ClaimResult {
    migration::require_backfill_funded(e);
    from.require_auth();
    require_emission_tier(e, tier);
    if pool_addresses.is_empty() {
        panic_with_error!(e, BackstopError::BadRequest);
    }
    if min_lp_tokens_out < 0 {
        panic_with_error!(e, BackstopError::NegativeAmountError);
    }

    let mut blnd_amount = 0_i128;
    let mut claims = Map::<Address, i128>::new(e);
    for pool in pool_addresses.iter() {
        if claims.contains_key(pool.clone()) {
            panic_with_error!(e, BackstopError::BadRequest);
        }
        require_registered_pool(e, &pool);
        prepare_pool_weight_change(e, tier, &pool);

        let pool_claim = distributor::claim_emissions(e, tier, &pool, from);
        claims.set(pool.clone(), pool_claim);
        blnd_amount = checked_add(e, blnd_amount, pool_claim);
    }

    if blnd_amount == 0 {
        return ClaimResult {
            lp_amount: 0,
            allocations: vec![e],
        };
    }

    let mut ongoing = get_ongoing_emission_state(e);
    ongoing.backstop_claimed = checked_add(e, ongoing.backstop_claimed, blnd_amount);
    set_ongoing_emission_state(e, &ongoing);

    let backstop = e.current_contract_address();
    let blnd = storage::get_blnd_token(e);
    let lp_token = tier_token(e, tier);
    let blnd_client = TokenClient::new(e, &blnd);
    let lp_client = TokenClient::new(e, &lp_token);
    let blnd_before = blnd_client.balance(&backstop);
    let lp_before = lp_client.balance(&backstop);
    let approval_ledger = e
        .ledger()
        .sequence()
        .checked_div(100_000)
        .and_then(|period| period.checked_add(1))
        .and_then(|period| period.checked_mul(100_000))
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    let approval_args: Vec<Val> = vec![
        e,
        backstop.clone().into_val(e),
        lp_token.clone().into_val(e),
        blnd_amount.into_val(e),
        approval_ledger.into_val(e),
    ];
    e.authorize_as_current_contract(vec![
        e,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: blnd.clone(),
                fn_name: Symbol::new(e, "approve"),
                args: approval_args,
            },
            sub_invocations: vec![e],
        }),
    ]);
    let lp_amount = CometClient::new(e, &lp_token).dep_tokn_amt_in_get_lp_tokns_out(
        &blnd,
        &blnd_amount,
        &min_lp_tokens_out,
        &backstop,
    );
    let blnd_after = blnd_client.balance(&backstop);
    let lp_after = lp_client.balance(&backstop);
    if blnd_before.checked_sub(blnd_after) != Some(blnd_amount)
        || lp_after.checked_sub(lp_before) != Some(lp_amount)
        || lp_amount <= 0
    {
        panic_with_error!(e, BackstopError::BalanceError);
    }

    let mut allocations = vec![e];
    for pool in pool_addresses.iter() {
        let pool_claim = claims.get(pool.clone()).unwrap_or(0);
        let pool_lp_amount = proportional_floor(e, lp_amount, pool_claim, blnd_amount);
        if pool_lp_amount == 0 {
            continue;
        }
        let shares = credit_tier_shares(e, tier, from, &pool, pool_lp_amount);
        finish_pool_weight_change(e, tier, &pool);
        allocations.push_back((pool, pool_claim, pool_lp_amount, shares));
    }
    ClaimResult {
        lp_amount,
        allocations,
    }
}

fn require_emission_tier(e: &Env, tier: BackstopTier) {
    if tier == BackstopTier::Usdc {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
}

#[cfg(test)]
pub(crate) fn preview_user_emissions(
    e: &Env,
    tier: BackstopTier,
    user: &Address,
    pool: &Address,
) -> UserEmissionData {
    require_emission_tier(e, tier);
    distributor::preview_user_emissions(e, tier, pool, user)
}

#[cfg(test)]
pub(crate) fn preview_claim(
    e: &Env,
    tier: BackstopTier,
    user: &Address,
    pool_addresses: &Vec<Address>,
) -> i128 {
    require_emission_tier(e, tier);
    if pool_addresses.is_empty() {
        panic_with_error!(e, BackstopError::BadRequest);
    }

    let mut claimable = 0_i128;
    let mut pools = Map::<Address, ()>::new(e);
    for pool in pool_addresses.iter() {
        if pools.contains_key(pool.clone()) {
            panic_with_error!(e, BackstopError::BadRequest);
        }
        require_registered_pool(e, &pool);
        pools.set(pool.clone(), ());
        claimable = checked_add(
            e,
            claimable,
            preview_user_emissions(e, tier, user, &pool).accrued,
        );
    }
    claimable
}
