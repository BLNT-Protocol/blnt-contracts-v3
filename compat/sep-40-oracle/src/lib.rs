#![no_std]

use soroban_sdk::{contractclient, contracttype, Address, Env, Symbol, Vec};

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype(export = false)]
pub struct PriceData {
    pub price: i128,
    pub timestamp: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype(export = false)]
pub enum Asset {
    Stellar(Address),
    Other(Symbol),
}

#[contractclient(name = "PriceFeedClient")]
pub trait PriceFeedTrait {
    fn base(env: Env) -> Asset;
    fn assets(env: Env) -> Vec<Asset>;
    fn decimals(env: Env) -> u32;
    fn resolution(env: Env) -> u32;
    fn price(env: Env, asset: Asset, timestamp: u64) -> Option<PriceData>;
    fn prices(env: Env, asset: Asset, records: u32) -> Option<Vec<PriceData>>;
    fn lastprice(env: Env, asset: Asset) -> Option<PriceData>;
}

#[cfg(any(test, feature = "testutils"))]
pub mod testutils;
