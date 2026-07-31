use soroban_sdk::{Address, Env};

mod backstop_contract_wasm {
    soroban_sdk::contractimport!(file = "../target/wasm32v1-none/optimized/backstop.wasm");
}
use backstop::{BackstopClient, BackstopContract};
use mock_backstop_valuation::{
    BackstopValuationBinding, MockBackstopValuation, MockBackstopValuationClient,
};

pub fn create_mock_backstop_valuation(
    e: &Env,
    blnd: &Address,
    blnd_usdc: &Address,
    blnd_xlm: &Address,
    usdc: &Address,
) -> Address {
    e.register(
        MockBackstopValuation,
        (BackstopValuationBinding {
            blnd: blnd.clone(),
            blnd_usdc: blnd_usdc.clone(),
            blnd_xlm: blnd_xlm.clone(),
            usdc: usdc.clone(),
        },),
    )
}

pub fn set_mock_backstop_valuation_failure(e: &Env, valuation: &Address, fail: bool) {
    MockBackstopValuationClient::new(e, valuation).set_quote_failure(&fail);
}

pub fn create_backstop<'a>(
    e: &Env,
    contract_id: &Address,
    wasm: bool,
    blnd_usdc_token: &Address,
    blnd_xlm_token: &Address,
    emitter: &Address,
    blnd_token: &Address,
    usdc_token: &Address,
    pool_factory: &Address,
) -> BackstopClient<'a> {
    let backstop_valuation =
        create_mock_backstop_valuation(e, blnd_token, blnd_usdc_token, blnd_xlm_token, usdc_token);
    if wasm {
        e.register_at(
            contract_id,
            backstop_contract_wasm::WASM,
            (
                blnd_usdc_token,
                blnd_xlm_token,
                emitter,
                blnd_token,
                usdc_token,
                pool_factory,
                backstop_valuation.clone(),
            ),
        );
    } else {
        e.register_at(
            contract_id,
            BackstopContract {},
            (
                blnd_usdc_token,
                blnd_xlm_token,
                emitter,
                blnd_token,
                usdc_token,
                pool_factory,
                backstop_valuation,
            ),
        );
    }
    BackstopClient::new(e, &contract_id)
}
