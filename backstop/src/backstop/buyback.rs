use crate::{
    backstop::{asset_token, BackstopAsset},
    constants::{
        BUYBACK_MAX_PRICE_DENOMINATOR, BUYBACK_MAX_PRICE_NUMERATOR,
        BUYBACK_MAX_RESERVE_DENOMINATOR, BUYBACK_MAX_RESERVE_NUMERATOR, SCALAR_7,
    },
    dependencies::CometClient,
    storage, BackstopError,
};
use sep_41_token::TokenClient;
use soroban_fixed_point_math::FixedPoint;
use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    panic_with_error, vec, Env, IntoVal, Symbol, Val, Vec,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuybackResult {
    pub blnd_burned: i128,
    pub pair_in: i128,
    pub pending: i128,
}

/// Convert one bounded pending USDC or XLM batch through its canonical Comet
/// and burn the exact BLND received. No-work calls return zeroes.
pub fn execute_buy_and_burn(e: &Env, asset: BackstopAsset) -> BuybackResult {
    let comet_address = match asset {
        BackstopAsset::Usdc => storage::get_blnd_usdc_token(e),
        BackstopAsset::Xlm => storage::get_blnd_xlm_token(e),
        _ => panic_with_error!(e, BackstopError::BadRequest),
    };
    let pending = storage::get_buyback_pending(e, asset);
    if pending <= 0 {
        return no_work(pending);
    }

    let backstop = e.current_contract_address();
    let pair = asset_token(e, asset);
    let blnd = storage::get_blnd_token(e);
    let comet = CometClient::new(e, &comet_address);

    let pair_reserve = comet.get_balance(&pair);
    if pair_reserve <= 0 {
        panic_with_error!(e, BackstopError::InvalidValuation);
    }
    let batch_limit = pair_reserve
        .fixed_mul_floor(
            BUYBACK_MAX_RESERVE_NUMERATOR,
            BUYBACK_MAX_RESERVE_DENOMINATOR,
        )
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    let pair_in = core::cmp::min(pending, batch_limit);
    if pair_in <= 0 {
        return no_work(pending);
    }

    let spot_price = comet.get_spot_price(&pair, &blnd);
    if spot_price <= 0 {
        panic_with_error!(e, BackstopError::InvalidValuation);
    }
    let max_price = spot_price
        .fixed_mul_ceil(BUYBACK_MAX_PRICE_NUMERATOR, BUYBACK_MAX_PRICE_DENOMINATOR)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    let min_blnd_out = pair_in
        .fixed_mul_floor(SCALAR_7, max_price)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    if min_blnd_out <= 0 {
        return no_work(pending);
    }

    let pair_client = TokenClient::new(e, &pair);
    let blnd_client = TokenClient::new(e, &blnd);
    let pair_before = pair_client.balance(&backstop);
    let blnd_before = blnd_client.balance(&backstop);
    // Contract auth applies to the next sub-contract call, so keep the
    // authorization immediately adjacent to the Comet invocation.
    authorize_comet_input(e, &backstop, &pair, &comet_address, pair_in);
    let (reported_blnd_out, reported_spot_price) =
        comet.swap_exact_amount_in(&pair, &pair_in, &blnd, &min_blnd_out, &max_price, &backstop);
    let pair_after = pair_client.balance(&backstop);
    let blnd_after_swap = blnd_client.balance(&backstop);
    let blnd_out = blnd_after_swap
        .checked_sub(blnd_before)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::BalanceError));
    if pair_before.checked_sub(pair_after) != Some(pair_in)
        || blnd_out <= 0
        || blnd_out < min_blnd_out
        || reported_blnd_out != blnd_out
        || reported_spot_price <= 0
        || reported_spot_price > max_price
    {
        panic_with_error!(e, BackstopError::BalanceError);
    }

    blnd_client.burn(&backstop, &blnd_out);
    if blnd_client.balance(&backstop) != blnd_before {
        panic_with_error!(e, BackstopError::BalanceError);
    }

    let next_pending = pending
        .checked_sub(pair_in)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    storage::set_buyback_pending(e, asset, next_pending);
    BuybackResult {
        blnd_burned: blnd_out,
        pair_in,
        pending: next_pending,
    }
}

fn authorize_comet_input(
    e: &Env,
    backstop: &soroban_sdk::Address,
    token: &soroban_sdk::Address,
    comet: &soroban_sdk::Address,
    amount: i128,
) {
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
        comet.clone().into_val(e),
        amount.into_val(e),
        approval_ledger.into_val(e),
    ];
    e.authorize_as_current_contract(vec![
        e,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: token.clone(),
                fn_name: Symbol::new(e, "approve"),
                args: approval_args,
            },
            sub_invocations: vec![e],
        }),
    ]);
}

fn no_work(pending: i128) -> BuybackResult {
    BuybackResult {
        blnd_burned: 0,
        pair_in: 0,
        pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        backstop::{execute_deposit, execute_donate, BackstopTier},
        contract::BackstopClient,
        testutils::{create_backstop_with_real_comets, create_mock_pool_factory},
    };
    use sep_41_token::testutils::MockTokenClient;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::{vec, Address};

    #[test]
    fn buy_and_burn_swaps_a_bounded_batch_and_burns_exact_output() {
        let e = Env::default();
        // The shared real-Comet fixture mints initial reserves through
        // authenticated token calls. Production Comet authorization is
        // exercised without mocks by the native integration suite.
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited();

        let backstop = create_backstop_with_real_comets(&e);
        let pool = Address::generate(&e);
        let depositor = Address::generate(&e);
        let filler = Address::generate(&e);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        factory.set_pool(&pool);
        let (usdc, blnd, comet_address) = e.as_contract(&backstop, || {
            (
                storage::get_usdc_token(&e),
                storage::get_blnd_token(&e),
                storage::get_blnd_usdc_token(&e),
            )
        });
        let usdc_client = MockTokenClient::new(&e, &usdc);
        let blnd_client = MockTokenClient::new(&e, &blnd);
        usdc_client.mint(&depositor, &(100 * SCALAR_7));
        usdc_client.mint(&filler, &(100 * SCALAR_7));
        usdc_client.approve(
            &filler,
            &backstop,
            &(100 * SCALAR_7),
            &e.ledger().sequence().saturating_add(1_000),
        );

        e.as_contract(&backstop, || {
            execute_deposit(
                &e,
                BackstopTier::ThirdLoss,
                &depositor,
                &pool,
                100 * SCALAR_7,
            );
            let donation =
                execute_donate(&e, BackstopTier::ThirdLoss, &filler, &pool, 100 * SCALAR_7);
            assert_eq!(donation.credited, 99 * SCALAR_7);
            assert_eq!(donation.buyback, SCALAR_7);
        });

        let comet = CometClient::new(&e, &comet_address);
        let comet_usdc_before = comet.get_balance(&usdc);
        let comet_blnd_before = comet.get_balance(&blnd);
        let blnd_burned = BackstopClient::new(&e, &backstop).buy_and_burn(&BackstopAsset::Usdc);
        let expected_usdc_in = core::cmp::min(
            SCALAR_7,
            comet_usdc_before / BUYBACK_MAX_RESERVE_DENOMINATOR,
        );
        assert!(blnd_burned > 0);
        assert_eq!(
            comet.get_balance(&usdc) - comet_usdc_before,
            expected_usdc_in
        );
        assert_eq!(comet_blnd_before - comet.get_balance(&blnd), blnd_burned);
        assert_eq!(blnd_client.balance(&backstop), 0);
        e.as_contract(&backstop, || {
            assert_eq!(
                storage::get_buyback_pending(&e, BackstopAsset::Usdc),
                SCALAR_7 - expected_usdc_in
            );
            assert_eq!(
                storage::get_pool_balance_for_tier(&e, BackstopTier::ThirdLoss, &pool).tokens,
                199 * SCALAR_7
            );
        });
    }

    #[test]
    fn buy_and_burn_without_pending_usdc_is_a_noop() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        let backstop = create_backstop_with_real_comets(&e);
        assert_eq!(
            BackstopClient::new(&e, &backstop).buy_and_burn(&BackstopAsset::Usdc),
            0
        );
    }

    #[test]
    fn xlm_haircut_swaps_through_the_blnd_xlm_comet() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited();

        let backstop = create_backstop_with_real_comets(&e);
        let pool = Address::generate(&e);
        let depositor = Address::generate(&e);
        let filler = Address::generate(&e);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        factory.set_pool_config(
            &pool,
            &vec![
                &e,
                mock_pool_factory::BackstopTierConfig {
                    asset: mock_pool_factory::BackstopAsset::Xlm,
                    take_rate_weight: 1,
                },
            ],
        );
        let (xlm, blnd, comet_address) = e.as_contract(&backstop, || {
            (
                storage::get_xlm_token(&e),
                storage::get_blnd_token(&e),
                storage::get_blnd_xlm_token(&e),
            )
        });
        let xlm_client = MockTokenClient::new(&e, &xlm);
        let blnd_client = MockTokenClient::new(&e, &blnd);
        xlm_client.mint(&depositor, &(100 * SCALAR_7));
        xlm_client.mint(&filler, &(100 * SCALAR_7));
        xlm_client.approve(
            &filler,
            &backstop,
            &(100 * SCALAR_7),
            &e.ledger().sequence().saturating_add(1_000),
        );

        e.as_contract(&backstop, || {
            execute_deposit(
                &e,
                BackstopTier::FirstLoss,
                &depositor,
                &pool,
                100 * SCALAR_7,
            );
            let donation =
                execute_donate(&e, BackstopTier::FirstLoss, &filler, &pool, 100 * SCALAR_7);
            assert_eq!(donation.credited, 99 * SCALAR_7);
            assert_eq!(donation.buyback, SCALAR_7);
            assert_eq!(
                storage::get_buyback_pending(&e, BackstopAsset::Xlm),
                SCALAR_7
            );
        });

        let comet = CometClient::new(&e, &comet_address);
        let comet_xlm_before = comet.get_balance(&xlm);
        let comet_blnd_before = comet.get_balance(&blnd);
        let blnd_burned = BackstopClient::new(&e, &backstop).buy_and_burn(&BackstopAsset::Xlm);
        let expected_xlm_in =
            core::cmp::min(SCALAR_7, comet_xlm_before / BUYBACK_MAX_RESERVE_DENOMINATOR);
        assert!(blnd_burned > 0);
        assert_eq!(comet.get_balance(&xlm) - comet_xlm_before, expected_xlm_in);
        assert_eq!(comet_blnd_before - comet.get_balance(&blnd), blnd_burned);
        assert_eq!(blnd_client.balance(&backstop), 0);
        e.as_contract(&backstop, || {
            assert_eq!(
                storage::get_buyback_pending(&e, BackstopAsset::Xlm),
                SCALAR_7 - expected_xlm_in
            );
            assert_eq!(
                storage::get_pool_balance_for_tier(&e, BackstopTier::FirstLoss, &pool).tokens,
                199 * SCALAR_7
            );
        });
    }
}
