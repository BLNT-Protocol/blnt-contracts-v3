use soroban_sdk::{testutils::Address as _, Address, Env};

use sep_40_oracle::testutils::{MockPriceOracle, MockPriceOracleClient};

pub fn create_mock_oracle<'a>(e: &Env) -> (Address, MockPriceOracleClient<'a>) {
    let contract_id = Address::generate(e);
    e.register_at(&contract_id, MockPriceOracle, ());
    (
        contract_id.clone(),
        MockPriceOracleClient::new(e, &contract_id),
    )
}
