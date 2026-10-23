use soroban_sdk::{Address, Env};

mod backstop_contract_wasm {
    soroban_sdk::contractimport!(file = "../target/wasm32v1-none/optimized/backstop.wasm");
}
use backstop::{BackstopClient, BackstopContract};

pub fn create_backstop<'a>(
    e: &Env,
    contract_id: &Address,
    wasm: bool,
    blnt_usdc_token: &Address,
    blnt_xlm_token: &Address,
    emitter: &Address,
    blnt_token: &Address,
    usdc_token: &Address,
    xlm_token: &Address,
    pool_factory: &Address,
) -> BackstopClient<'a> {
    if wasm {
        e.register_at(
            contract_id,
            backstop_contract_wasm::WASM,
            (
                blnt_usdc_token,
                blnt_xlm_token,
                emitter,
                blnt_token,
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
                blnt_usdc_token,
                blnt_xlm_token,
                emitter,
                blnt_token,
                usdc_token,
                xlm_token,
                pool_factory,
                soroban_sdk::Vec::<(Address, i128)>::new(e),
            ),
        );
    }
    BackstopClient::new(e, &contract_id)
}
