use soroban_sdk::{contractclient, contracttype, Address, Env};

/// One immutable valuation for a backstop LP-token amount.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct AssetValuation {
    pub underlying_blnd: i128,
    pub usdc_value: i128,
    pub valid_until: u64,
}

/// Assets an immutable backstop valuation contract is authorized to price.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct BackstopValuationBinding {
    pub blnd: Address,
    pub blnd_usdc: Address,
    pub blnd_xlm: Address,
    pub usdc: Address,
}

#[contractclient(name = "BackstopValuationClient")]
#[allow(dead_code)]
pub trait BackstopValuation {
    fn binding(env: Env) -> BackstopValuationBinding;
    fn quote(env: Env, token: Address, amount: i128) -> AssetValuation;
}
