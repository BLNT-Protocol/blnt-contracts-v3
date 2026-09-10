use soroban_sdk::{panic_with_error, Address, Env, I256};

use crate::{
    backstop::{tier_for_token, BackstopTier},
    dependencies::CometClient,
    errors::BackstopError,
    storage,
};

const BACKSTOP_EMISSION_NUMERATOR: i128 = 7;
const POOL_EMISSION_NUMERATOR: i128 = 3;
const EMISSION_SPLIT_DENOMINATOR: i128 = 10;

/// Arithmetic-only allocation quote for one BLNT-emission scope.
#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct BlntEmissionQuote {
    allocation: i128,
    eligible_blnt: i128,
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct BlntEmissionValues {
    blnt_usdc: i128,
    blnt_xlm: i128,
}

#[cfg(test)]
fn quote_pool_blnt_emissions(
    e: &Env,
    distribution: i128,
    values: &BlntEmissionValues,
    total_reward_zone_blnt: i128,
    reward_zone_member: bool,
) -> BlntEmissionQuote {
    if distribution < 0 || total_reward_zone_blnt < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let pool_blnt = eligible_blnt(e, values);
    if !reward_zone_member {
        return BlntEmissionQuote {
            allocation: 0,
            eligible_blnt: pool_blnt,
        };
    }
    if pool_blnt > total_reward_zone_blnt {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let allocation = if pool_blnt == 0 {
        0
    } else {
        proportional_floor(e, distribution, pool_blnt, total_reward_zone_blnt)
    };
    BlntEmissionQuote {
        allocation,
        eligible_blnt: pool_blnt,
    }
}

#[cfg(test)]
fn quote_user_blnt_emissions(
    e: &Env,
    pool_distribution: i128,
    values: &BlntEmissionValues,
    pool_eligible_blnt: i128,
) -> BlntEmissionQuote {
    if pool_distribution < 0 || pool_eligible_blnt < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let user_blnt = eligible_blnt(e, values);
    if user_blnt > pool_eligible_blnt {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let allocation = if user_blnt == 0 {
        0
    } else {
        proportional_floor(e, pool_distribution, user_blnt, pool_eligible_blnt)
    };
    BlntEmissionQuote {
        allocation,
        eligible_blnt: user_blnt,
    }
}

pub(crate) fn pool_spot_blnt_emission_weight(e: &Env, pool: &Address) -> i128 {
    let blnt_usdc_token = storage::get_blnt_usdc_token(e);
    let blnt_xlm_token = storage::get_blnt_xlm_token(e);
    let blnt_usdc = spot_underlying_blnt_for_token(e, pool, &blnt_usdc_token);
    let blnt_xlm = spot_underlying_blnt_for_token(e, pool, &blnt_xlm_token);
    checked_add(e, blnt_usdc, blnt_xlm)
}

fn spot_underlying_blnt_for_token(e: &Env, pool: &Address, token: &Address) -> i128 {
    if let Some(tier) = tier_for_token(e, pool, token) {
        spot_underlying_blnt(e, token, pool_active_emission_assets(e, tier, pool))
    } else {
        0
    }
}

pub(crate) fn quote_ongoing_blnt_split(
    e: &Env,
    distribution: i128,
    prior_carry: i128,
) -> (i128, i128, i128) {
    if distribution < 0 || prior_carry < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let total = checked_add(e, distribution, prior_carry);
    let backstop = proportional_floor(
        e,
        total,
        BACKSTOP_EMISSION_NUMERATOR,
        EMISSION_SPLIT_DENOMINATOR,
    );
    let pool = proportional_floor(
        e,
        total,
        POOL_EMISSION_NUMERATOR,
        EMISSION_SPLIT_DENOMINATOR,
    );
    let allocated = checked_add(e, backstop, pool);
    (backstop, pool, checked_sub(e, total, allocated))
}

#[cfg(test)]
fn eligible_blnt(e: &Env, values: &BlntEmissionValues) -> i128 {
    if values.blnt_usdc < 0 || values.blnt_xlm < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    checked_add(e, values.blnt_usdc, values.blnt_xlm)
}

pub(crate) fn pool_active_emission_assets(e: &Env, tier: BackstopTier, pool: &Address) -> i128 {
    let balance = storage::get_pool_balance_for_tier(e, tier, pool);
    if balance.tokens < 0 || balance.shares < 0 || balance.q4w < 0 || balance.q4w > balance.shares {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let active_shares = checked_sub(e, balance.shares, balance.q4w);
    balance.convert_to_tokens(active_shares)
}

fn spot_underlying_blnt(e: &Env, token: &Address, lp_amount: i128) -> i128 {
    if lp_amount < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let (total_supply, blnt_reserve) = comet_composition(e, token);
    underlying_blnt_from_composition(e, lp_amount, total_supply, blnt_reserve)
}

pub(crate) fn comet_composition(e: &Env, token: &Address) -> (i128, i128) {
    if *token != storage::get_blnt_usdc_token(e) && *token != storage::get_blnt_xlm_token(e) {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let comet = CometClient::new(e, token);
    let total_supply = comet.get_total_supply();
    let blnt_reserve = comet.get_balance(&storage::get_blnt_token(e));
    if total_supply <= 0 || blnt_reserve < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    (total_supply, blnt_reserve)
}

pub(crate) fn underlying_blnt_from_composition(
    e: &Env,
    lp_amount: i128,
    total_supply: i128,
    blnt_reserve: i128,
) -> i128 {
    if lp_amount < 0 || total_supply <= 0 || blnt_reserve < 0 || lp_amount > total_supply {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    proportional_floor(e, lp_amount, blnt_reserve, total_supply)
}

fn checked_add(e: &Env, left: i128, right: i128) -> i128 {
    left.checked_add(right)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

fn checked_sub(e: &Env, left: i128, right: i128) -> i128 {
    left.checked_sub(right)
        .filter(|result| *result >= 0)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

pub(crate) fn proportional_floor(e: &Env, value: i128, numerator: i128, denominator: i128) -> i128 {
    if value < 0 || numerator < 0 || denominator <= 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    I256::from_i128(e, value)
        .mul(&I256::from_i128(e, numerator))
        .div(&I256::from_i128(e, denominator))
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

#[cfg(test)]
mod tests {
    extern crate std;

    use crate::constants::SCALAR_7;

    use super::*;

    #[test]
    fn ongoing_split_conserves_carry() {
        let e = Env::default();
        let first = quote_ongoing_blnt_split(&e, 11, 0);
        assert_eq!(first, (7, 3, 1));

        assert_eq!(quote_ongoing_blnt_split(&e, 9, first.2), (7, 3, 0));
    }

    #[test]
    fn pool_quote_uses_both_blnt_tiers_and_membership() {
        let e = Env::default();
        let values = BlntEmissionValues {
            blnt_usdc: 60 * SCALAR_7,
            blnt_xlm: 40 * SCALAR_7,
        };

        assert_eq!(
            quote_pool_blnt_emissions(&e, 1_000 * SCALAR_7, &values, 400 * SCALAR_7, true,),
            BlntEmissionQuote {
                allocation: 250 * SCALAR_7,
                eligible_blnt: 100 * SCALAR_7,
            }
        );
        assert_eq!(
            quote_pool_blnt_emissions(&e, 1_000 * SCALAR_7, &values, 400 * SCALAR_7, false,),
            BlntEmissionQuote {
                allocation: 0,
                eligible_blnt: 100 * SCALAR_7,
            }
        );
    }

    #[test]
    fn user_quote_uses_both_blnt_tiers() {
        let e = Env::default();
        assert_eq!(
            quote_user_blnt_emissions(
                &e,
                200 * SCALAR_7,
                &BlntEmissionValues {
                    blnt_usdc: 15 * SCALAR_7,
                    blnt_xlm: 10 * SCALAR_7,
                },
                100 * SCALAR_7,
            ),
            BlntEmissionQuote {
                allocation: 50 * SCALAR_7,
                eligible_blnt: 25 * SCALAR_7,
            }
        );
    }

    #[test]
    fn policy_rejects_invalid_values_and_handles_wide_products() {
        let e = Env::default();
        assert_eq!(
            quote_pool_blnt_emissions(
                &e,
                i128::MAX,
                &BlntEmissionValues {
                    blnt_usdc: i128::MAX,
                    blnt_xlm: 0,
                },
                i128::MAX,
                true,
            )
            .allocation,
            i128::MAX
        );

        let negative = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            quote_user_blnt_emissions(
                &e,
                SCALAR_7,
                &BlntEmissionValues {
                    blnt_usdc: -1,
                    blnt_xlm: 0,
                },
                SCALAR_7,
            )
        }));
        assert!(negative.is_err());

        let invalid_total = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            quote_pool_blnt_emissions(
                &e,
                SCALAR_7,
                &BlntEmissionValues {
                    blnt_usdc: 2,
                    blnt_xlm: 0,
                },
                1,
                true,
            )
        }));
        assert!(invalid_total.is_err());

        let overflow = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            quote_ongoing_blnt_split(&e, i128::MAX, 1)
        }));
        assert!(overflow.is_err());
    }
}
