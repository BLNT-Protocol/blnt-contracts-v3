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
    policy::proportional_floor,
    tier_accounting::{
        checked_add, finish_pool_weight_change, get_ongoing_emission_state,
        prepare_pool_weight_change, set_ongoing_emission_state,
    },
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
#[cfg(test)]
mod tests {

    use crate::{
        backstop::{BackstopTier, PoolBalance, UserBalance},
        storage::{BackstopEmissionData, OngoingEmissionState, UserEmissionData},
        testutils::{
            create_backstop_with_real_comets as create_raw_backstop, create_blnd_token,
            create_comet_lp_pool, create_usdc_token,
        },
    };

    use super::*;
    use mock_pool_factory::MockPoolFactoryClient;
    use soroban_sdk::{
        testutils::{Address as _, Ledger, LedgerInfo},
        unwrap::UnwrapOptimized,
        vec,
    };

    fn create_backstop(e: &Env) -> Address {
        let backstop = create_raw_backstop(e);
        e.as_contract(&backstop, || {
            crate::migration::activate_for_test(e, e.ledger().timestamp());
            crate::storage::set_ongoing_emission_state(
                e,
                &OngoingEmissionState {
                    backstop_allocated: i128::MAX / 4,
                    backstop_carry: 0,
                    backstop_claimed: 0,
                    last_distribution: Some(e.ledger().timestamp()),
                    pool_allocated: 0,
                    pool_carry: 0,
                    split_carry: 0,
                    total_distributed: i128::MAX / 4,
                },
            );
        });
        backstop
    }

    fn execute_claim(
        e: &Env,
        from: &Address,
        pools: &Vec<Address>,
        min_lp_tokens_out: &i128,
    ) -> i128 {
        super::execute_claim(e, BackstopTier::BlndUsdc, from, pools, *min_lp_tokens_out).lp_amount
    }

    mod storage {
        use super::*;

        fn register_pool(e: &Env, pool: &Address) {
            let factory = crate::storage::get_pool_factory(e);
            MockPoolFactoryClient::new(e, &factory).set_pool(pool);
        }

        pub fn set_backstop_emis_data(e: &Env, pool: &Address, data: &BackstopEmissionData) {
            crate::storage::set_backstop_emis_data(e, BackstopTier::BlndUsdc, pool, data);
        }

        pub fn get_backstop_emis_data(e: &Env, pool: &Address) -> Option<BackstopEmissionData> {
            crate::storage::get_backstop_emis_data(e, BackstopTier::BlndUsdc, pool)
        }

        pub fn set_user_emis_data(
            e: &Env,
            pool: &Address,
            user: &Address,
            data: &UserEmissionData,
        ) {
            crate::storage::set_user_emis_data(e, BackstopTier::BlndUsdc, pool, user, data);
        }

        pub fn get_user_emis_data(
            e: &Env,
            pool: &Address,
            user: &Address,
        ) -> Option<UserEmissionData> {
            crate::storage::get_user_emis_data(e, BackstopTier::BlndUsdc, pool, user)
        }

        pub fn set_pool_balance(e: &Env, pool: &Address, balance: &PoolBalance) {
            register_pool(e, pool);
            crate::storage::set_pool_balance_for_tier(e, BackstopTier::BlndUsdc, pool, balance);
        }

        pub fn get_pool_balance(e: &Env, pool: &Address) -> PoolBalance {
            crate::storage::get_pool_balance_for_tier(e, BackstopTier::BlndUsdc, pool)
        }

        pub fn set_user_balance(e: &Env, pool: &Address, user: &Address, balance: &UserBalance) {
            crate::storage::set_user_balance_for_tier(
                e,
                BackstopTier::BlndUsdc,
                pool,
                user,
                balance,
            );
        }

        pub fn get_user_balance(e: &Env, pool: &Address, user: &Address) -> UserBalance {
            crate::storage::get_user_balance_for_tier(e, BackstopTier::BlndUsdc, pool, user)
        }

        pub fn set_backstop_token(e: &Env, token: &Address) {
            crate::storage::set_blnd_usdc_token(e, token);
        }

        pub fn set_blnd_token(e: &Env, token: &Address) {
            crate::storage::set_blnd_token(e, token);
        }
    }

    /********** claim **********/

    #[test]
    fn test_claim() {
        let e = Env::default();
        e.mock_all_auths();
        let block_timestamp = 1500000000 + 12345;
        e.ledger().set(LedgerInfo {
            timestamp: block_timestamp,
            protocol_version: 27,
            sequence_number: 0,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });
        e.cost_estimate().budget().reset_unlimited();

        let backstop_address = create_backstop(&e);
        let pool_1_id = Address::generate(&e);
        let pool_2_id = Address::generate(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd_address, blnd_token_client) = create_blnd_token(&e, &backstop_address, &bombadil);
        let (usdc_address, _) = create_usdc_token(&e, &backstop_address, &bombadil);
        blnd_token_client.mint(&backstop_address, &100_0000000);

        let backstop_1_emissions_data = BackstopEmissionData {
            expiration: 1500000000 + 7 * 24 * 60 * 60,
            eps: 0_10000000000000,
            index: 222220000000,
            last_time: 1500000000,
            index_carry: 0,
            schedule_carry: 0,
        };
        let user_1_emissions_data = UserEmissionData {
            index: 111110000000,
            accrued: 1_2345678,
            carry: 0,
        };

        let backstop_2_emissions_data = BackstopEmissionData {
            expiration: 1500000000 + 7 * 24 * 60 * 60,
            eps: 0_02000000000000,
            index: 0,
            last_time: 1500010000,
            index_carry: 0,
            schedule_carry: 0,
        };
        let user_2_emissions_data = UserEmissionData {
            index: 0,
            accrued: 0,
            carry: 0,
        };
        let (lp_address, lp_client) =
            create_comet_lp_pool(&e, &bombadil, &blnd_address, &usdc_address);
        e.as_contract(&backstop_address, || {
            storage::set_backstop_emis_data(&e, &pool_1_id, &backstop_1_emissions_data);
            storage::set_user_emis_data(&e, &pool_1_id, &samwise, &user_1_emissions_data);
            storage::set_backstop_emis_data(&e, &pool_2_id, &backstop_2_emissions_data);
            storage::set_user_emis_data(&e, &pool_2_id, &samwise, &user_2_emissions_data);
            storage::set_backstop_token(&e, &lp_address);
            storage::set_blnd_token(&e, &blnd_address);
            storage::set_pool_balance(
                &e,
                &pool_1_id,
                &PoolBalance {
                    shares: 150_0000000,
                    tokens: 200_0000000,
                    q4w: 2_0000000,
                },
            );
            storage::set_user_balance(
                &e,
                &pool_1_id,
                &samwise,
                &UserBalance {
                    shares: 9_0000000,
                    q4w: vec![&e],
                },
            );
            storage::set_pool_balance(
                &e,
                &pool_2_id,
                &PoolBalance {
                    shares: 70_0000000,
                    tokens: 75_0000000,
                    q4w: 3_5000000,
                },
            );
            storage::set_user_balance(
                &e,
                &pool_2_id,
                &samwise,
                &UserBalance {
                    shares: 7_5000000,
                    q4w: vec![&e],
                },
            );
            let backstop_lp_balance = lp_client.balance(&backstop_address);
            let pre_pool_tokens_1 = storage::get_pool_balance(&e, &pool_1_id).tokens;
            let pre_pool_tokens_2 = storage::get_pool_balance(&e, &pool_2_id).tokens;
            let pre_pool_shares_1 = storage::get_pool_balance(&e, &pool_1_id).shares;
            let pre_pool_shares_2 = storage::get_pool_balance(&e, &pool_2_id).shares;
            let result = execute_claim(
                &e,
                &samwise,
                &vec![&e, pool_1_id.clone(), pool_2_id.clone()],
                &6_4000000,
            );
            assert_eq!(result, 6_4729327);
            assert_eq!(
                lp_client.balance(&backstop_address),
                backstop_lp_balance + 6_4729327
            );
            assert_eq!(
                blnd_token_client.balance(&backstop_address),
                100_0000000 - (76_3155136 + 5_2894736)
            );
            let sam_balance_1 = storage::get_user_balance(&e, &pool_1_id, &samwise);
            assert_eq!(sam_balance_1.shares, 9_0000000 + 4_5400275);
            let sam_balance_2 = storage::get_user_balance(&e, &pool_2_id, &samwise);
            assert_eq!(sam_balance_2.shares, 7_5000000 + 0_3915917);

            let pool_balance_1 = storage::get_pool_balance(&e, &pool_1_id);
            assert_eq!(pool_balance_1.tokens, pre_pool_tokens_1 + 6_0533700);
            assert_eq!(pool_balance_1.shares, pre_pool_shares_1 + 4_5400275);
            let pool_balance_2 = storage::get_pool_balance(&e, &pool_2_id);
            assert_eq!(pool_balance_2.tokens, pre_pool_tokens_2 + 0_4195626);
            assert_eq!(pool_balance_2.shares, pre_pool_shares_2 + 0_3915917);

            let new_backstop_1_data =
                storage::get_backstop_emis_data(&e, &pool_1_id).unwrap_optimized();
            let new_user_1_data =
                storage::get_user_emis_data(&e, &pool_1_id, &samwise).unwrap_optimized();
            assert_eq!(new_backstop_1_data.last_time, block_timestamp);
            assert_eq!(new_backstop_1_data.index, 834343841621621);
            assert_eq!(new_user_1_data.accrued, 0);
            assert_eq!(new_user_1_data.index, 834343841621621);

            let new_backstop_2_data =
                storage::get_backstop_emis_data(&e, &pool_2_id).unwrap_optimized();
            let new_user_2_data =
                storage::get_user_emis_data(&e, &pool_2_id, &samwise).unwrap_optimized();
            assert_eq!(new_backstop_2_data.last_time, block_timestamp);
            assert_eq!(new_backstop_2_data.index, 70526315789473);
            assert_eq!(new_user_2_data.accrued, 0);
            assert_eq!(new_user_2_data.index, 70526315789473);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #20)")]
    fn test_claim_uses_min_lp_amount() {
        let e = Env::default();
        e.mock_all_auths();
        let block_timestamp = 1500000000 + 12345;
        e.ledger().set(LedgerInfo {
            timestamp: block_timestamp,
            protocol_version: 27,
            sequence_number: 0,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });
        e.cost_estimate().budget().reset_unlimited();

        let backstop_address = create_backstop(&e);
        let pool_1_id = Address::generate(&e);
        let pool_2_id = Address::generate(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd_address, blnd_token_client) = create_blnd_token(&e, &backstop_address, &bombadil);
        let (usdc_address, _) = create_usdc_token(&e, &backstop_address, &bombadil);
        blnd_token_client.mint(&backstop_address, &100_0000000);

        let backstop_1_emissions_data = BackstopEmissionData {
            expiration: 1500000000 + 7 * 24 * 60 * 60,
            eps: 0_10000000000000,
            index: 222220000000,
            last_time: 1500000000,
            index_carry: 0,
            schedule_carry: 0,
        };
        let user_1_emissions_data = UserEmissionData {
            index: 111110000000,
            accrued: 1_2345678,
            carry: 0,
        };

        let backstop_2_emissions_data = BackstopEmissionData {
            expiration: 1500000000 + 7 * 24 * 60 * 60,
            eps: 0_02000000000000,
            index: 0,
            last_time: 1500010000,
            index_carry: 0,
            schedule_carry: 0,
        };
        let user_2_emissions_data = UserEmissionData {
            index: 0,
            accrued: 0,
            carry: 0,
        };
        let (lp_address, _) = create_comet_lp_pool(&e, &bombadil, &blnd_address, &usdc_address);
        e.as_contract(&backstop_address, || {
            storage::set_backstop_emis_data(&e, &pool_1_id, &backstop_1_emissions_data);
            storage::set_user_emis_data(&e, &pool_1_id, &samwise, &user_1_emissions_data);
            storage::set_backstop_emis_data(&e, &pool_2_id, &backstop_2_emissions_data);
            storage::set_user_emis_data(&e, &pool_2_id, &samwise, &user_2_emissions_data);
            storage::set_backstop_token(&e, &lp_address);
            storage::set_blnd_token(&e, &blnd_address);
            storage::set_pool_balance(
                &e,
                &pool_1_id,
                &PoolBalance {
                    shares: 150_0000000,
                    tokens: 200_0000000,
                    q4w: 2_0000000,
                },
            );
            storage::set_user_balance(
                &e,
                &pool_1_id,
                &samwise,
                &UserBalance {
                    shares: 9_0000000,
                    q4w: vec![&e],
                },
            );
            storage::set_pool_balance(
                &e,
                &pool_2_id,
                &PoolBalance {
                    shares: 70_0000000,
                    tokens: 75_0000000,
                    q4w: 3_5000000,
                },
            );
            storage::set_user_balance(
                &e,
                &pool_2_id,
                &samwise,
                &UserBalance {
                    shares: 7_5000000,
                    q4w: vec![&e],
                },
            );
            execute_claim(
                &e,
                &samwise,
                &vec![&e, pool_1_id.clone(), pool_2_id.clone()],
                &6_5000000,
            );
        });
    }

    #[test]
    fn test_claim_twice() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths();

        let block_timestamp = 1500000000 + 12345;
        e.ledger().set(LedgerInfo {
            timestamp: block_timestamp,
            protocol_version: 27,
            sequence_number: 0,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let backstop_address = create_backstop(&e);
        let pool_1_id = Address::generate(&e);
        let pool_2_id = Address::generate(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd_address, blnd_token_client) = create_blnd_token(&e, &backstop_address, &bombadil);
        let (usdc_address, _) = create_usdc_token(&e, &backstop_address, &bombadil);
        blnd_token_client.mint(&backstop_address, &300_0000000);

        let backstop_1_emissions_data = BackstopEmissionData {
            expiration: 1500000000 + 7 * 24 * 60 * 60,
            eps: 0_10000000000000,
            index: 222220000000,
            last_time: 1500000000,
            index_carry: 0,
            schedule_carry: 0,
        };
        let user_1_emissions_data = UserEmissionData {
            index: 111110000000,
            accrued: 1_2345678,
            carry: 0,
        };

        let backstop_2_emissions_data = BackstopEmissionData {
            expiration: 1500000000 + 7 * 24 * 60 * 60,
            eps: 0_02000000000000,
            index: 0,
            last_time: 1500010000,
            index_carry: 0,
            schedule_carry: 0,
        };
        let user_2_emissions_data = UserEmissionData {
            index: 0,
            accrued: 0,
            carry: 0,
        };
        let (lp_address, lp_client) =
            create_comet_lp_pool(&e, &bombadil, &blnd_address, &usdc_address);
        e.as_contract(&backstop_address, || {
            storage::set_backstop_emis_data(&e, &pool_1_id, &backstop_1_emissions_data);
            storage::set_user_emis_data(&e, &pool_1_id, &samwise, &user_1_emissions_data);
            storage::set_backstop_emis_data(&e, &pool_2_id, &backstop_2_emissions_data);
            storage::set_user_emis_data(&e, &pool_2_id, &samwise, &user_2_emissions_data);
            storage::set_backstop_token(&e, &lp_address);
            storage::set_blnd_token(&e, &blnd_address);
            storage::set_pool_balance(
                &e,
                &pool_1_id,
                &PoolBalance {
                    shares: 150_0000000,
                    tokens: 200_0000000,
                    q4w: 2_0000000,
                },
            );
            storage::set_user_balance(
                &e,
                &pool_1_id,
                &samwise,
                &UserBalance {
                    shares: 9_0000000,
                    q4w: vec![&e],
                },
            );
            storage::set_pool_balance(
                &e,
                &pool_2_id,
                &PoolBalance {
                    shares: 70_0000000,
                    tokens: 75_0000000,
                    q4w: 3_5000000,
                },
            );
            storage::set_user_balance(
                &e,
                &pool_2_id,
                &samwise,
                &UserBalance {
                    shares: 7_5000000,
                    q4w: vec![&e],
                },
            );
            let backstop_lp_balance = lp_client.balance(&backstop_address);
            let pre_pool_tokens_1 = storage::get_pool_balance(&e, &pool_1_id).tokens;
            let pre_pool_tokens_2 = storage::get_pool_balance(&e, &pool_2_id).tokens;
            let pre_pool_shares_1 = storage::get_pool_balance(&e, &pool_1_id).shares;
            let pre_pool_shares_2 = storage::get_pool_balance(&e, &pool_2_id).shares;
            let result = execute_claim(
                &e,
                &samwise,
                &vec![&e, pool_1_id.clone(), pool_2_id.clone()],
                &6_4000000,
            );
            assert_eq!(result, 6_4729327);
            assert_eq!(
                lp_client.balance(&backstop_address),
                backstop_lp_balance + 6_4729327
            );
            assert_eq!(
                blnd_token_client.balance(&backstop_address),
                300_0000000 - (76_3155136 + 5_2894736)
            );
            let sam_balance_1 = storage::get_user_balance(&e, &pool_1_id, &samwise);
            assert_eq!(sam_balance_1.shares, 9_0000000 + 4_5400275);
            let sam_balance_2 = storage::get_user_balance(&e, &pool_2_id, &samwise);
            assert_eq!(sam_balance_2.shares, 7_5000000 + 0_3915917);

            let pool_balance_1 = storage::get_pool_balance(&e, &pool_1_id);
            assert_eq!(pool_balance_1.tokens, pre_pool_tokens_1 + 6_0533700);
            assert_eq!(pool_balance_1.shares, pre_pool_shares_1 + 4_5400275);
            let pool_balance_2 = storage::get_pool_balance(&e, &pool_2_id);
            assert_eq!(pool_balance_2.tokens, pre_pool_tokens_2 + 0_4195626);
            assert_eq!(pool_balance_2.shares, pre_pool_shares_2 + 0_3915917);

            let new_backstop_1_data =
                storage::get_backstop_emis_data(&e, &pool_1_id).unwrap_optimized();
            let new_user_1_data =
                storage::get_user_emis_data(&e, &pool_1_id, &samwise).unwrap_optimized();
            assert_eq!(new_backstop_1_data.last_time, block_timestamp);
            assert_eq!(new_backstop_1_data.index, 834343841621621);
            assert_eq!(new_user_1_data.accrued, 0);
            assert_eq!(new_user_1_data.index, 834343841621621);

            let new_backstop_2_data =
                storage::get_backstop_emis_data(&e, &pool_2_id).unwrap_optimized();
            let new_user_2_data =
                storage::get_user_emis_data(&e, &pool_2_id, &samwise).unwrap_optimized();
            assert_eq!(new_backstop_2_data.last_time, block_timestamp);
            assert_eq!(new_backstop_2_data.index, 70526315789473);
            assert_eq!(new_user_2_data.accrued, 0);
            assert_eq!(new_user_2_data.index, 70526315789473);
        });
        e.as_contract(&backstop_address, || {
            let block_timestamp_1 = 1500000000 + 12345 + 12345;
            e.ledger().set(LedgerInfo {
                timestamp: block_timestamp_1,
                protocol_version: 27,
                sequence_number: 0,
                network_id: Default::default(),
                base_reserve: 10,
                min_temp_entry_ttl: 10,
                min_persistent_entry_ttl: 10,
                max_entry_ttl: 3110400,
            });
            let backstop_lp_balance = lp_client.balance(&backstop_address);
            let pre_samwise_balance_1 = storage::get_user_balance(&e, &pool_1_id, &samwise).shares;
            let pre_samwise_balance_2 = storage::get_user_balance(&e, &pool_2_id, &samwise).shares;
            let pre_pool_tokens_1 = storage::get_pool_balance(&e, &pool_1_id).tokens;
            let pre_pool_tokens_2 = storage::get_pool_balance(&e, &pool_2_id).tokens;
            let pre_pool_shares_1 = storage::get_pool_balance(&e, &pool_1_id).shares;
            let pre_pool_shares_2 = storage::get_pool_balance(&e, &pool_2_id).shares;
            let result_1 = execute_claim(
                &e,
                &samwise,
                &vec![&e, pool_1_id.clone(), pool_2_id.clone()],
                &10_7000000,
            );
            assert_eq!(result_1, 10_7836702);
            // V3 carries the two sub-token units that v2's floor rounding discarded.
            assert_eq!(
                blnd_token_client.balance(&backstop_address),
                300_0000000 - (109_5788706 + 29_1282348) - (76_3155136 + 5_2894736) - 2
            );
            assert_eq!(
                lp_client.balance(&backstop_address),
                backstop_lp_balance + 8_5191194 + 2_2645507 + 1
            );
            let sam_balance_1 = storage::get_user_balance(&e, &pool_1_id, &samwise);
            assert_eq!(sam_balance_1.shares, pre_samwise_balance_1 + 6_3893395);
            let sam_balance_2 = storage::get_user_balance(&e, &pool_2_id, &samwise);
            assert_eq!(sam_balance_2.shares, pre_samwise_balance_2 + 2_1135806);

            let pool_balance_1 = storage::get_pool_balance(&e, &pool_1_id);
            assert_eq!(pool_balance_1.tokens, pre_pool_tokens_1 + 8_5191194);
            assert_eq!(pool_balance_1.shares, pre_pool_shares_1 + 6_3893395);
            let pool_balance_2 = storage::get_pool_balance(&e, &pool_2_id);
            assert_eq!(pool_balance_2.tokens, pre_pool_tokens_2 + 2_2645507);
            assert_eq!(pool_balance_2.shares, pre_pool_shares_2 + 2_1135806);
            let new_backstop_1_data =
                storage::get_backstop_emis_data(&e, &pool_1_id).unwrap_optimized();
            let new_user_1_data =
                storage::get_user_emis_data(&e, &pool_1_id, &samwise).unwrap_optimized();
            assert_eq!(new_backstop_1_data.last_time, block_timestamp_1);
            assert_eq!(new_backstop_1_data.index, 1643639618102322);
            assert_eq!(new_user_1_data.accrued, 0);
            assert_eq!(new_user_1_data.index, 1643639618102322);

            let new_backstop_2_data =
                storage::get_backstop_emis_data(&e, &pool_2_id).unwrap_optimized();
            let new_user_2_data =
                storage::get_user_emis_data(&e, &pool_2_id, &samwise).unwrap_optimized();
            assert_eq!(new_backstop_2_data.last_time, block_timestamp_1);
            assert_eq!(new_backstop_2_data.index, 439631002529944);
            assert_eq!(new_user_2_data.accrued, 0);
            assert_eq!(new_user_2_data.index, 439631002529944);
        });
    }

    #[test]
    fn test_claim_no_deposits() {
        let e = Env::default();
        e.mock_all_auths();
        let block_timestamp = 1500000000 + 12345;
        e.ledger().set(LedgerInfo {
            timestamp: block_timestamp,
            protocol_version: 27,
            sequence_number: 0,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let backstop_address = create_backstop(&e);
        let pool_1_id = Address::generate(&e);
        let pool_2_id = Address::generate(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let frodo = Address::generate(&e);

        let (_, blnd_token_client) = create_blnd_token(&e, &backstop_address, &bombadil);
        blnd_token_client.mint(&backstop_address, &100_0000000);

        let backstop_1_emissions_data = BackstopEmissionData {
            expiration: 1500000000 + 7 * 24 * 60 * 60,
            eps: 0_10000000000000,
            index: 222220000000,
            last_time: 1500000000,
            index_carry: 0,
            schedule_carry: 0,
        };

        let backstop_2_emissions_data = BackstopEmissionData {
            expiration: 1500000000 + 7 * 24 * 60 * 60,
            eps: 0_02000000000000,
            index: 0,
            last_time: 1500010000,
            index_carry: 0,
            schedule_carry: 0,
        };
        e.as_contract(&backstop_address, || {
            storage::set_backstop_emis_data(&e, &pool_1_id, &backstop_1_emissions_data);
            storage::set_backstop_emis_data(&e, &pool_2_id, &backstop_2_emissions_data);

            storage::set_pool_balance(
                &e,
                &pool_1_id,
                &PoolBalance {
                    shares: 150_0000000,
                    tokens: 200_0000000,
                    q4w: 0,
                },
            );
            storage::set_pool_balance(
                &e,
                &pool_2_id,
                &PoolBalance {
                    shares: 70_0000000,
                    tokens: 75_0000000,
                    q4w: 0,
                },
            );

            let result = execute_claim(
                &e,
                &samwise,
                &vec![&e, pool_1_id.clone(), pool_2_id.clone()],
                &0,
            );
            assert_eq!(result, 0);
            assert_eq!(blnd_token_client.balance(&frodo), 0);
            assert_eq!(blnd_token_client.balance(&backstop_address), 100_0000000);

            let new_backstop_1_data =
                storage::get_backstop_emis_data(&e, &pool_1_id).unwrap_optimized();
            let new_user_1_data =
                storage::get_user_emis_data(&e, &pool_1_id, &samwise).unwrap_optimized();
            assert_eq!(new_backstop_1_data.last_time, block_timestamp);
            assert_eq!(new_backstop_1_data.index, 823222220000000);
            assert_eq!(new_user_1_data.accrued, 0);
            assert_eq!(new_user_1_data.index, 823222220000000);

            let new_backstop_2_data =
                storage::get_backstop_emis_data(&e, &pool_2_id).unwrap_optimized();
            let new_user_2_data =
                storage::get_user_emis_data(&e, &pool_2_id, &samwise).unwrap_optimized();
            assert_eq!(new_backstop_2_data.last_time, block_timestamp);
            assert_eq!(new_backstop_2_data.index, 67000000000000);
            assert_eq!(new_user_2_data.accrued, 0);
            assert_eq!(new_user_2_data.index, 67000000000000);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1000)")]
    fn test_claim_duplicate() {
        let e = Env::default();
        e.mock_all_auths();
        let block_timestamp = 1500000000 + 12345;
        e.ledger().set(LedgerInfo {
            timestamp: block_timestamp,
            protocol_version: 27,
            sequence_number: 0,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });
        e.cost_estimate().budget().reset_unlimited();

        let backstop_address = create_backstop(&e);
        let pool_1_id = Address::generate(&e);
        let pool_2_id = Address::generate(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd_address, blnd_token_client) = create_blnd_token(&e, &backstop_address, &bombadil);
        let (usdc_address, _) = create_usdc_token(&e, &backstop_address, &bombadil);
        blnd_token_client.mint(&backstop_address, &100_0000000);

        let backstop_1_emissions_data = BackstopEmissionData {
            expiration: 1500000000 + 7 * 24 * 60 * 60,
            eps: 0_10000000000000,
            index: 222220000000,
            last_time: 1500000000,
            index_carry: 0,
            schedule_carry: 0,
        };
        let user_1_emissions_data = UserEmissionData {
            index: 111110000000,
            accrued: 1_2345678,
            carry: 0,
        };

        let backstop_2_emissions_data = BackstopEmissionData {
            expiration: 1500000000 + 7 * 24 * 60 * 60,
            eps: 0_02000000000000,
            index: 0,
            last_time: 1500010000,
            index_carry: 0,
            schedule_carry: 0,
        };
        let user_2_emissions_data = UserEmissionData {
            index: 0,
            accrued: 0,
            carry: 0,
        };
        let (lp_address, _) = create_comet_lp_pool(&e, &bombadil, &blnd_address, &usdc_address);
        e.as_contract(&backstop_address, || {
            storage::set_backstop_emis_data(&e, &pool_1_id, &backstop_1_emissions_data);
            storage::set_user_emis_data(&e, &pool_1_id, &samwise, &user_1_emissions_data);
            storage::set_backstop_emis_data(&e, &pool_2_id, &backstop_2_emissions_data);
            storage::set_user_emis_data(&e, &pool_2_id, &samwise, &user_2_emissions_data);
            storage::set_backstop_token(&e, &lp_address);
            storage::set_blnd_token(&e, &blnd_address);
            storage::set_pool_balance(
                &e,
                &pool_1_id,
                &PoolBalance {
                    shares: 150_0000000,
                    tokens: 200_0000000,
                    q4w: 2_0000000,
                },
            );
            storage::set_user_balance(
                &e,
                &pool_1_id,
                &samwise,
                &UserBalance {
                    shares: 9_0000000,
                    q4w: vec![&e],
                },
            );
            storage::set_pool_balance(
                &e,
                &pool_2_id,
                &PoolBalance {
                    shares: 70_0000000,
                    tokens: 75_0000000,
                    q4w: 3_5000000,
                },
            );
            storage::set_user_balance(
                &e,
                &pool_2_id,
                &samwise,
                &UserBalance {
                    shares: 7_5000000,
                    q4w: vec![&e],
                },
            );
            execute_claim(
                &e,
                &samwise,
                &vec![&e, pool_1_id.clone(), pool_2_id.clone(), pool_1_id.clone()],
                &6_4000000,
            );
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1000)")]
    fn test_claim_empty() {
        let e = Env::default();
        e.mock_all_auths();
        let block_timestamp = 1500000000 + 12345;
        e.ledger().set(LedgerInfo {
            timestamp: block_timestamp,
            protocol_version: 27,
            sequence_number: 0,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });
        e.cost_estimate().budget().reset_unlimited();

        let backstop_address = create_backstop(&e);
        let pool_1_id = Address::generate(&e);
        let pool_2_id = Address::generate(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd_address, blnd_token_client) = create_blnd_token(&e, &backstop_address, &bombadil);
        let (usdc_address, _) = create_usdc_token(&e, &backstop_address, &bombadil);
        blnd_token_client.mint(&backstop_address, &100_0000000);

        let backstop_1_emissions_data = BackstopEmissionData {
            expiration: 1500000000 + 7 * 24 * 60 * 60,
            eps: 0_10000000000000,
            index: 222220000000,
            last_time: 1500000000,
            index_carry: 0,
            schedule_carry: 0,
        };
        let user_1_emissions_data = UserEmissionData {
            index: 111110000000,
            accrued: 1_2345678,
            carry: 0,
        };

        let backstop_2_emissions_data = BackstopEmissionData {
            expiration: 1500000000 + 7 * 24 * 60 * 60,
            eps: 0_02000000000000,
            index: 0,
            last_time: 1500010000,
            index_carry: 0,
            schedule_carry: 0,
        };
        let user_2_emissions_data = UserEmissionData {
            index: 0,
            accrued: 0,
            carry: 0,
        };
        let (lp_address, _) = create_comet_lp_pool(&e, &bombadil, &blnd_address, &usdc_address);
        e.as_contract(&backstop_address, || {
            storage::set_backstop_emis_data(&e, &pool_1_id, &backstop_1_emissions_data);
            storage::set_user_emis_data(&e, &pool_1_id, &samwise, &user_1_emissions_data);
            storage::set_backstop_emis_data(&e, &pool_2_id, &backstop_2_emissions_data);
            storage::set_user_emis_data(&e, &pool_2_id, &samwise, &user_2_emissions_data);
            storage::set_backstop_token(&e, &lp_address);
            storage::set_blnd_token(&e, &blnd_address);
            storage::set_pool_balance(
                &e,
                &pool_1_id,
                &PoolBalance {
                    shares: 150_0000000,
                    tokens: 200_0000000,
                    q4w: 2_0000000,
                },
            );
            storage::set_user_balance(
                &e,
                &pool_1_id,
                &samwise,
                &UserBalance {
                    shares: 9_0000000,
                    q4w: vec![&e],
                },
            );
            storage::set_pool_balance(
                &e,
                &pool_2_id,
                &PoolBalance {
                    shares: 70_0000000,
                    tokens: 75_0000000,
                    q4w: 3_5000000,
                },
            );
            storage::set_user_balance(
                &e,
                &pool_2_id,
                &samwise,
                &UserBalance {
                    shares: 7_5000000,
                    q4w: vec![&e],
                },
            );
            execute_claim(&e, &samwise, &vec![&e], &6_4000000);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1004)")]
    fn test_claim_random_adddress() {
        let e = Env::default();
        e.mock_all_auths();
        let block_timestamp = 1500000000 + 12345;
        e.ledger().set(LedgerInfo {
            timestamp: block_timestamp,
            protocol_version: 27,
            sequence_number: 0,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });
        e.cost_estimate().budget().reset_unlimited();

        let backstop_address = create_backstop(&e);
        let pool_1_id = Address::generate(&e);
        let pool_2_id = Address::generate(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd_address, blnd_token_client) = create_blnd_token(&e, &backstop_address, &bombadil);
        let (usdc_address, _) = create_usdc_token(&e, &backstop_address, &bombadil);
        blnd_token_client.mint(&backstop_address, &100_0000000);

        let backstop_1_emissions_data = BackstopEmissionData {
            expiration: 1500000000 + 7 * 24 * 60 * 60,
            eps: 0_10000000000000,
            index: 222220000000,
            last_time: 1500000000,
            index_carry: 0,
            schedule_carry: 0,
        };
        let user_1_emissions_data = UserEmissionData {
            index: 111110000000,
            accrued: 1_2345678,
            carry: 0,
        };

        let backstop_2_emissions_data = BackstopEmissionData {
            expiration: 1500000000 + 7 * 24 * 60 * 60,
            eps: 0_02000000000000,
            index: 0,
            last_time: 1500010000,
            index_carry: 0,
            schedule_carry: 0,
        };
        let user_2_emissions_data = UserEmissionData {
            index: 0,
            accrued: 0,
            carry: 0,
        };
        let (lp_address, _) = create_comet_lp_pool(&e, &bombadil, &blnd_address, &usdc_address);
        e.as_contract(&backstop_address, || {
            storage::set_backstop_emis_data(&e, &pool_1_id, &backstop_1_emissions_data);
            storage::set_user_emis_data(&e, &pool_1_id, &samwise, &user_1_emissions_data);
            storage::set_backstop_emis_data(&e, &pool_2_id, &backstop_2_emissions_data);
            storage::set_user_emis_data(&e, &pool_2_id, &samwise, &user_2_emissions_data);
            storage::set_backstop_token(&e, &lp_address);
            storage::set_blnd_token(&e, &blnd_address);
            storage::set_pool_balance(
                &e,
                &pool_1_id,
                &PoolBalance {
                    shares: 150_0000000,
                    tokens: 200_0000000,
                    q4w: 2_0000000,
                },
            );
            storage::set_user_balance(
                &e,
                &pool_1_id,
                &samwise,
                &UserBalance {
                    shares: 9_0000000,
                    q4w: vec![&e],
                },
            );
            storage::set_pool_balance(
                &e,
                &pool_2_id,
                &PoolBalance {
                    shares: 70_0000000,
                    tokens: 75_0000000,
                    q4w: 3_5000000,
                },
            );
            storage::set_user_balance(
                &e,
                &pool_2_id,
                &samwise,
                &UserBalance {
                    shares: 7_5000000,
                    q4w: vec![&e],
                },
            );
            execute_claim(
                &e,
                &samwise,
                &vec![&e, pool_1_id.clone(), Address::generate(&e)],
                &1,
            );
        });
    }
}
