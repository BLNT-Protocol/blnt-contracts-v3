use soroban_sdk::{Address, Env};

use backstop::EmitterClient;
use mock_emitter::MockEmitter;

pub fn create_emitter<'a>(e: &Env) -> (Address, EmitterClient<'a>) {
    let contract_id = e.register(MockEmitter, ());
    (contract_id.clone(), EmitterClient::new(e, &contract_id))
}
