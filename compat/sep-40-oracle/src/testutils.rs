use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, Address, Env, Vec,
};

pub use crate::{Asset, PriceData};

const LEDGER_THRESHOLD: u32 = 30 * 17_280;
const LEDGER_BUMP: u32 = 31 * 17_280;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PriceOracleError {
    AssetMissing = 2,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Admin,
    Assets,
    AssetIndex(Asset),
    Base,
    Decimals,
    LastTimestamp,
    Resolution,
}

pub trait MockPriceFeed {
    fn set_data(
        env: Env,
        admin: Address,
        base: Asset,
        assets: Vec<Asset>,
        decimals: u32,
        resolution: u32,
    );
    fn set_price(env: Env, prices: Vec<i128>, timestamp: u64);
    fn set_price_stable(env: Env, prices: Vec<i128>);
    fn base(env: Env) -> Asset;
    fn assets(env: Env) -> Vec<Asset>;
    fn decimals(env: Env) -> u32;
    fn resolution(env: Env) -> u32;
    fn price(env: Env, asset: Asset, timestamp: u64) -> Option<PriceData>;
    fn prices(env: Env, asset: Asset, records: u32) -> Option<Vec<PriceData>>;
    fn lastprice(env: Env, asset: Asset) -> Option<PriceData>;
}

#[contract]
pub struct MockPriceOracle;

#[contractimpl]
impl MockPriceFeed for MockPriceOracle {
    fn set_data(
        env: Env,
        admin: Address,
        base: Asset,
        assets: Vec<Asset>,
        decimals: u32,
        resolution: u32,
    ) {
        if let Some(old_admin) = env.storage().instance().get::<_, Address>(&DataKey::Admin) {
            old_admin.require_auth();
        } else {
            admin.require_auth();
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Base, &base);
        env.storage().instance().set(&DataKey::Assets, &assets);
        env.storage().instance().set(&DataKey::Decimals, &decimals);
        env.storage()
            .instance()
            .set(&DataKey::Resolution, &resolution);
        for (index, asset) in assets.iter().enumerate() {
            env.storage()
                .instance()
                .set(&DataKey::AssetIndex(asset), &(index as u32));
        }
        extend_instance(&env);
    }

    fn set_price(env: Env, prices: Vec<i128>, timestamp: u64) {
        require_admin(&env);
        extend_instance(&env);
        env.storage()
            .temporary()
            .set(&DataKey::LastTimestamp, &timestamp);
        set_prices(&env, prices, timestamp);
    }

    fn set_price_stable(env: Env, prices: Vec<i128>) {
        require_admin(&env);
        extend_instance(&env);
        env.storage()
            .temporary()
            .set(&DataKey::LastTimestamp, &0_u64);
        set_prices(&env, prices, 0);
    }

    fn base(env: Env) -> Asset {
        env.storage().instance().get(&DataKey::Base).unwrap()
    }

    fn assets(env: Env) -> Vec<Asset> {
        env.storage()
            .instance()
            .get(&DataKey::Assets)
            .unwrap_or_else(|| Vec::new(&env))
    }

    fn decimals(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::Decimals).unwrap()
    }

    fn resolution(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::Resolution).unwrap()
    }

    fn price(env: Env, asset: Asset, timestamp: u64) -> Option<PriceData> {
        price_data(&env, &asset, timestamp)
    }

    fn prices(env: Env, asset: Asset, records: u32) -> Option<Vec<PriceData>> {
        let mut result = Vec::new(&env);
        let resolution = Self::resolution(env.clone()) as u64;
        let mut timestamp = last_timestamp(&env);
        if timestamp == 0 || resolution == 0 {
            return None;
        }
        let records = records.min(20);
        for _ in 0..records {
            let Some(value) = price_data(&env, &asset, timestamp) else {
                break;
            };
            result.push_back(value);
            timestamp = timestamp.saturating_sub(resolution);
        }
        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    }

    fn lastprice(env: Env, asset: Asset) -> Option<PriceData> {
        price_data(&env, &asset, last_timestamp(&env))
    }
}

fn extend_instance(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(LEDGER_THRESHOLD, LEDGER_BUMP);
}

fn require_admin(env: &Env) {
    let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
    admin.require_auth();
}

fn last_timestamp(env: &Env) -> u64 {
    env.storage()
        .temporary()
        .get(&DataKey::LastTimestamp)
        .unwrap_or(0)
}

fn asset_index(env: &Env, asset: &Asset) -> u8 {
    env.storage()
        .instance()
        .get::<_, u32>(&DataKey::AssetIndex(asset.clone()))
        .unwrap_or_else(|| panic_with_error!(env, PriceOracleError::AssetMissing)) as u8
}

fn set_prices(env: &Env, prices: Vec<i128>, timestamp: u64) {
    for (index, price) in prices.iter().enumerate() {
        let key = price_key(index as u8, timestamp);
        env.storage().temporary().set(&key, &price);
        env.storage()
            .temporary()
            .extend_ttl(&key, LEDGER_BUMP, LEDGER_BUMP);
    }
    env.storage()
        .temporary()
        .extend_ttl(&DataKey::LastTimestamp, LEDGER_BUMP, LEDGER_BUMP);
}

fn price_data(env: &Env, asset: &Asset, timestamp: u64) -> Option<PriceData> {
    let key = price_key(asset_index(env, asset), timestamp);
    let price = env.storage().temporary().get::<_, i128>(&key)?;
    if timestamp == 0 {
        env.storage()
            .temporary()
            .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);
    }
    Some(PriceData {
        price,
        timestamp: if timestamp == 0 {
            env.ledger().timestamp()
        } else {
            timestamp
        },
    })
}

fn price_key(asset_index: u8, timestamp: u64) -> u128 {
    (u128::from(timestamp) << 64) | u128::from(asset_index)
}
