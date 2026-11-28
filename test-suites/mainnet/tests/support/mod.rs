use kanatoko::multihost::Session;
use soroban_sdk::{
    contracttype, vec,
    xdr::{FromXdr, ToXdr},
    Address, Bytes, Env, IntoVal, Val, Vec,
};

#[allow(clippy::too_many_arguments)]
pub mod pool {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/optimized/pool.wasm");
}

pub const POOL: &str = "CAUTPSQDAJEUV5XEU3EXFVYRGDJLEJGVTFJCFZCSPTDCREAMZ573VF4D";
pub const ORACLE: &str = "CDN7JD6SP32RZHB3DHK4S4AWCCQF6GYIUO455UCR4RGRZILZDRD24N36";
const REFLECTOR: &str = "CALI2BYU2JE6WVRUFYTS6MSBNEHGJ35P4AVCZYF3B6QOE3QKOB2PLE6M";
pub const XLM: &str = "CAS3J7GYLGXMF6TDJBBYYSE3HQ6BBSMLNUQ34T6TZMYMW2EVH34XOWMA";
pub const USDC: &str = "CCW67TSZV3SSS2HXMBQ5JFGCKJNXKZM7UQUWUZPUTHXSTZLEO7SJMI75";
pub const PINS: &[(&str, Option<&str>)] = &[
    (
        POOL,
        Some("0e4f8f45b94e6b0d017a496ac5620ad0e3f866533cf45acd2b070bedda1e7356"),
    ),
    (
        ORACLE,
        Some("41df04894bf2d8291492326f7a6ab9c90a90cc9828e31990ce8aeeab42680bfd"),
    ),
    (REFLECTOR, None),
];
pub const UNIT: i128 = 10_000_000;
pub const RATE_SCALE: i128 = 1_000_000_000_000;

#[derive(Clone, Debug, PartialEq)]
#[contracttype]
enum Asset {
    Stellar(Address),
}

#[derive(Clone, Debug, PartialEq)]
#[contracttype]
pub struct PriceData {
    pub price: i128,
    pub timestamp: u64,
}

#[repr(u32)]
#[derive(Clone, Copy)]
pub enum Operation {
    Supply = 2,
    Withdraw = 3,
    Borrow = 4,
    Repay = 5,
}

pub struct Market<'a> {
    fork: &'a dyn Session,
    codec: Env,
    borrower: Address,
}
impl<'a> Market<'a> {
    pub fn new(fork: &'a dyn Session) -> Self {
        let codec = Env::default();
        let borrower = Address::from_str(
            &codec,
            &fork.mock_funded_source_account("blnt-oracle-outage-borrower-v1", 1000 * UNIT as i64),
        );
        let market = Self {
            fork,
            codec,
            borrower,
        };
        let _: () = market.call(USDC, "trust", (&market.borrower,));
        let config: pool::PoolConfig = market.call(POOL, "get_config", ());
        assert_eq!(config.oracle, market.address(ORACLE));
        market
    }
    fn address(&self, id: &str) -> Address {
        Address::from_str(&self.codec, id)
    }
    fn try_call<T: FromXdr>(
        &self,
        id: &str,
        function: &str,
        args: impl IntoVal<Env, Vec<Val>>,
    ) -> Result<T, String> {
        let args = args.into_val(&self.codec).to_xdr(&self.codec);
        let reply = self
            .fork
            .call(id, function, &args.iter().collect::<std::vec::Vec<_>>())?;
        T::from_xdr(&self.codec, &Bytes::from_slice(&self.codec, &reply))
            .map_err(|_| format!("invalid SDK 27 result for {function}"))
    }
    fn call<T: FromXdr>(&self, id: &str, function: &str, args: impl IntoVal<Env, Vec<Val>>) -> T {
        self.try_call(id, function, args)
            .unwrap_or_else(|error| panic!("{function}: {error}"))
    }
    pub fn price(&self, token: &str) -> Option<PriceData> {
        self.call(ORACLE, "lastprice", (Asset::Stellar(self.address(token)),))
    }
    pub fn max_age(&self) -> u64 {
        self.call(ORACLE, "max_age", ())
    }
    pub fn submit(&self, requests: &[(Operation, &str, i128)]) -> Result<pool::Positions, String> {
        let mut batch = vec![&self.codec];
        for &(op, token, amount) in requests {
            batch.push_back(pool::Request {
                request_type: op as u32,
                address: self.address(token),
                amount,
            });
        }
        self.try_call(
            POOL,
            "submit",
            (&self.borrower, &self.borrower, &self.borrower, batch),
        )
    }
    pub fn positions(&self) -> pool::Positions {
        self.call(POOL, "get_positions", (&self.borrower,))
    }
    pub fn reserve(&self, token: &str) -> pool::Reserve {
        self.call(POOL, "get_reserve", (self.address(token),))
    }
    pub fn wallet(&self, token: &str) -> i128 {
        self.call(token, "balance", (&self.borrower,))
    }
    pub fn custody(&self, token: &str) -> i128 {
        self.call(token, "balance", (self.address(POOL),))
    }
}
