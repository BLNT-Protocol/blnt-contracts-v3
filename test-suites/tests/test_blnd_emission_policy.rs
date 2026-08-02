#![cfg(test)]

use backstop::{BackstopTier, BlndEmissionQuote, BlndEmissionValues, OngoingBlndSplit};
use test_suites::{
    liquidity_pool::LPClient,
    test_fixture::{TestFixture, TokenIndex, SCALAR_7},
};

fn exercise_blnd_emission_policy(wasm: bool) {
    let fixture = TestFixture::create(wasm);
    let e = fixture.env.clone();

    assert_eq!(
        fixture.backstop.quote_ongoing_blnd_split(&11, &0),
        OngoingBlndSplit {
            backstop: 7,
            carry: 1,
            pool: 3,
            total: 11,
        }
    );

    let pool_values = BlndEmissionValues {
        blnd_usdc: 60 * SCALAR_7,
        blnd_xlm: 40 * SCALAR_7,
    };
    assert_eq!(
        fixture.backstop.quote_pool_blnd_emissions(
            &(1_000 * SCALAR_7),
            &pool_values,
            &(400 * SCALAR_7),
            &true,
        ),
        BlndEmissionQuote {
            allocation: 250 * SCALAR_7,
            eligible_blnd: 100 * SCALAR_7,
        }
    );
    assert_eq!(
        fixture.backstop.quote_user_blnd_emissions(
            &(250 * SCALAR_7),
            &BlndEmissionValues {
                blnd_usdc: 10 * SCALAR_7,
                blnd_xlm: 15 * SCALAR_7,
            },
            &(100 * SCALAR_7),
        ),
        BlndEmissionQuote {
            allocation: 625_000_000,
            eligible_blnd: 25 * SCALAR_7,
        }
    );

    let blnd_xlm_token = fixture.backstop.backstop_token(&BackstopTier::BlndXlm);
    let blnd_xlm = LPClient::new(&e, &blnd_xlm_token);
    let blnd_usdc_amount = fixture.lp.get_total_supply() / 10;
    let blnd_xlm_amount = blnd_xlm.get_total_supply() / 5;
    let values = fixture
        .backstop
        .spot_blnd_emission_values(&blnd_usdc_amount, &blnd_xlm_amount);

    assert_eq!(
        values,
        BlndEmissionValues {
            blnd_usdc: fixture.tokens[TokenIndex::BLND].balance(&fixture.lp.address) / 10,
            blnd_xlm: fixture.tokens[TokenIndex::BLND].balance(&blnd_xlm_token) / 5,
        }
    );
    assert!(fixture
        .backstop
        .try_spot_blnd_emission_values(&-1, &0)
        .is_err());
    assert!(fixture
        .backstop
        .try_spot_blnd_emission_values(&(fixture.lp.get_total_supply() + 1), &0)
        .is_err());
}

#[test]
fn native_blnd_emission_policy() {
    exercise_blnd_emission_policy(false);
}

#[test]
fn optimized_wasm_blnd_emission_policy() {
    exercise_blnd_emission_policy(true);
}
