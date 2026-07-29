use soroban_sdk::{contracttype, panic_with_error, Address, Env, I256};

use crate::{
    backstop::{tier_token, BackstopTier, BlndEmissionValues},
    dependencies::CometClient,
    errors::BackstopError,
    storage,
};

const BACKSTOP_EMISSION_NUMERATOR: i128 = 7;
const POOL_EMISSION_NUMERATOR: i128 = 3;
const EMISSION_SPLIT_DENOMINATOR: i128 = 10;

/// Arithmetic-only allocation quote for one BLND-emission scope.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct BlndEmissionQuote {
    pub allocation: i128,
    pub eligible_blnd: i128,
}

/// Immutable top-level split of ongoing BLND received from the emitter.
///
/// `carry` remains at this scope for the next split.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct OngoingBlndSplit {
    pub backstop: i128,
    pub carry: i128,
    pub pool: i128,
    pub total: i128,
}

pub(crate) fn quote_pool_blnd_emissions(
    e: &Env,
    distribution: i128,
    values: &BlndEmissionValues,
    total_reward_zone_blnd: i128,
    reward_zone_member: bool,
) -> BlndEmissionQuote {
    if distribution < 0 || total_reward_zone_blnd < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let pool_blnd = eligible_blnd(e, values);
    if !reward_zone_member {
        return BlndEmissionQuote {
            allocation: 0,
            eligible_blnd: pool_blnd,
        };
    }
    if pool_blnd > total_reward_zone_blnd {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let allocation = if pool_blnd == 0 {
        0
    } else {
        proportional_floor(e, distribution, pool_blnd, total_reward_zone_blnd)
    };
    BlndEmissionQuote {
        allocation,
        eligible_blnd: pool_blnd,
    }
}

pub(crate) fn quote_user_blnd_emissions(
    e: &Env,
    pool_distribution: i128,
    values: &BlndEmissionValues,
    pool_eligible_blnd: i128,
) -> BlndEmissionQuote {
    if pool_distribution < 0 || pool_eligible_blnd < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let user_blnd = eligible_blnd(e, values);
    if user_blnd > pool_eligible_blnd {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let allocation = if user_blnd == 0 {
        0
    } else {
        proportional_floor(e, pool_distribution, user_blnd, pool_eligible_blnd)
    };
    BlndEmissionQuote {
        allocation,
        eligible_blnd: user_blnd,
    }
}

pub(crate) fn spot_blnd_emission_values(
    e: &Env,
    blnd_usdc_lp: i128,
    blnd_xlm_lp: i128,
) -> BlndEmissionValues {
    BlndEmissionValues {
        blnd_usdc: spot_underlying_blnd(e, BackstopTier::BlndUsdc, blnd_usdc_lp),
        blnd_xlm: spot_underlying_blnd(e, BackstopTier::BlndXlm, blnd_xlm_lp),
    }
}

pub(crate) fn pool_spot_blnd_emission_values(e: &Env, pool: &Address) -> BlndEmissionValues {
    BlndEmissionValues {
        blnd_usdc: spot_underlying_blnd(
            e,
            BackstopTier::BlndUsdc,
            pool_active_emission_assets(e, BackstopTier::BlndUsdc, pool),
        ),
        blnd_xlm: spot_underlying_blnd(
            e,
            BackstopTier::BlndXlm,
            pool_active_emission_assets(e, BackstopTier::BlndXlm, pool),
        ),
    }
}

pub(crate) fn quote_ongoing_blnd_split(
    e: &Env,
    distribution: i128,
    prior_carry: i128,
) -> OngoingBlndSplit {
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
    OngoingBlndSplit {
        backstop,
        carry: checked_sub(e, total, allocated),
        pool,
        total,
    }
}

pub(crate) fn eligible_blnd(e: &Env, values: &BlndEmissionValues) -> i128 {
    if values.blnd_usdc < 0 || values.blnd_xlm < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    checked_add(e, values.blnd_usdc, values.blnd_xlm)
}

fn pool_active_emission_assets(e: &Env, tier: BackstopTier, pool: &Address) -> i128 {
    let balance = storage::get_pool_balance_for_tier(e, tier, pool);
    if balance.tokens < 0 || balance.shares < 0 || balance.q4w < 0 || balance.q4w > balance.shares {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let active_shares = checked_sub(e, balance.shares, balance.q4w);
    balance.convert_to_tokens(active_shares)
}

fn spot_underlying_blnd(e: &Env, tier: BackstopTier, lp_amount: i128) -> i128 {
    if lp_amount < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let comet = CometClient::new(e, &tier_token(e, tier));
    let total_supply = comet.get_total_supply();
    let blnd_reserve = comet.get_balance(&storage::get_blnd_token(e));
    if total_supply <= 0 || blnd_reserve < 0 || lp_amount > total_supply {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    proportional_floor(e, lp_amount, blnd_reserve, total_supply)
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

fn proportional_floor(e: &Env, value: i128, numerator: i128, denominator: i128) -> i128 {
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
        let first = quote_ongoing_blnd_split(&e, 11, 0);
        assert_eq!(
            first,
            OngoingBlndSplit {
                backstop: 7,
                carry: 1,
                pool: 3,
                total: 11,
            }
        );

        assert_eq!(
            quote_ongoing_blnd_split(&e, 9, first.carry),
            OngoingBlndSplit {
                backstop: 7,
                carry: 0,
                pool: 3,
                total: 10,
            }
        );
    }

    #[test]
    fn pool_quote_uses_both_blnd_tiers_and_membership() {
        let e = Env::default();
        let values = BlndEmissionValues {
            blnd_usdc: 60 * SCALAR_7,
            blnd_xlm: 40 * SCALAR_7,
        };

        assert_eq!(
            quote_pool_blnd_emissions(&e, 1_000 * SCALAR_7, &values, 400 * SCALAR_7, true,),
            BlndEmissionQuote {
                allocation: 250 * SCALAR_7,
                eligible_blnd: 100 * SCALAR_7,
            }
        );
        assert_eq!(
            quote_pool_blnd_emissions(&e, 1_000 * SCALAR_7, &values, 400 * SCALAR_7, false,),
            BlndEmissionQuote {
                allocation: 0,
                eligible_blnd: 100 * SCALAR_7,
            }
        );
    }

    #[test]
    fn user_quote_uses_both_blnd_tiers() {
        let e = Env::default();
        assert_eq!(
            quote_user_blnd_emissions(
                &e,
                200 * SCALAR_7,
                &BlndEmissionValues {
                    blnd_usdc: 15 * SCALAR_7,
                    blnd_xlm: 10 * SCALAR_7,
                },
                100 * SCALAR_7,
            ),
            BlndEmissionQuote {
                allocation: 50 * SCALAR_7,
                eligible_blnd: 25 * SCALAR_7,
            }
        );
    }

    #[test]
    fn policy_rejects_invalid_values_and_handles_wide_products() {
        let e = Env::default();
        assert_eq!(
            quote_pool_blnd_emissions(
                &e,
                i128::MAX,
                &BlndEmissionValues {
                    blnd_usdc: i128::MAX,
                    blnd_xlm: 0,
                },
                i128::MAX,
                true,
            )
            .allocation,
            i128::MAX
        );

        let negative = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            quote_user_blnd_emissions(
                &e,
                SCALAR_7,
                &BlndEmissionValues {
                    blnd_usdc: -1,
                    blnd_xlm: 0,
                },
                SCALAR_7,
            )
        }));
        assert!(negative.is_err());

        let invalid_total = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            quote_pool_blnd_emissions(
                &e,
                SCALAR_7,
                &BlndEmissionValues {
                    blnd_usdc: 2,
                    blnd_xlm: 0,
                },
                1,
                true,
            )
        }));
        assert!(invalid_total.is_err());

        let overflow = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            quote_ongoing_blnd_split(&e, i128::MAX, 1)
        }));
        assert!(overflow.is_err());
    }
}
