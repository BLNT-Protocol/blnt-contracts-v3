use soroban_sdk::{Address, Env};

mod backstop_contract_wasm {
    soroban_sdk::contractimport!(file = "../target/wasm32v1-none/optimized/backstop.wasm");
}
use backstop::{BackstopClient, BackstopContract};

pub fn create_backstop<'a>(
    e: &Env,
    contract_id: &Address,
    wasm: bool,
    blnd_usdc_token: &Address,
    blnd_xlm_token: &Address,
    emitter: &Address,
    blnd_token: &Address,
    usdc_token: &Address,
    xlm_token: &Address,
    pool_factory: &Address,
) -> BackstopClient<'a> {
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
                xlm_token,
                pool_factory,
                soroban_sdk::Vec::<(Address, i128)>::new(e),
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
                xlm_token,
                pool_factory,
                soroban_sdk::Vec::<(Address, i128)>::new(e),
            ),
        );
    }
    BackstopClient::new(e, &contract_id)
}
