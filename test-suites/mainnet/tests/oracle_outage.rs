use std::{cell::RefCell, path::Path};

use kanatoko::{
    canonical_ledger_digest, mainnet,
    soroban_sdk::{self, contracttype, testutils::Ledger, vec, xdr, Address},
    CapturedFixture, InvokeErrorKind, ScenarioFork,
};
use sha2::{Digest, Sha256};

// Generated bindings retain the protocol's existing public API signatures.
#[allow(clippy::too_many_arguments)]
mod pool {
    use kanatoko::soroban_sdk;
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/optimized/pool.wasm");
}

#[derive(Clone, Debug, PartialEq)]
#[contracttype]
pub enum Asset {
    Stellar(Address),
}

#[derive(Clone, Debug, PartialEq)]
#[contracttype]
pub struct PriceData {
    pub price: i128,
    pub timestamp: u64,
}

const POOL: &str = "CAUTPSQDAJEUV5XEU3EXFVYRGDJLEJGVTFJCFZCSPTDCREAMZ573VF4D";
const ORACLE: &str = "CDN7JD6SP32RZHB3DHK4S4AWCCQF6GYIUO455UCR4RGRZILZDRD24N36";
const REFLECTOR: &str = "CALI2BYU2JE6WVRUFYTS6MSBNEHGJ35P4AVCZYF3B6QOE3QKOB2PLE6M";
const XLM: &str = "CAS3J7GYLGXMF6TDJBBYYSE3HQ6BBSMLNUQ34T6TZMYMW2EVH34XOWMA";
const USDC: &str = "CCW67TSZV3SSS2HXMBQ5JFGCKJNXKZM7UQUWUZPUTHXSTZLEO7SJMI75";
const NETWORK: &str = "Public Global Stellar Network ; September 2015";
const SUPPLY_COLLATERAL: u32 = 2;
const WITHDRAW_COLLATERAL: u32 = 3;
const BORROW: u32 = 4;
const REPAY: u32 = 5;
const EXTRA_BORROW: i128 = 100_000; // 0.01 USDC
const PARTIAL_REPAY: i128 = 5_000_000; // 0.5 USDC

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn validate_capture(path: &Path) -> CapturedFixture {
    let captured = CapturedFixture::from_file(path, NETWORK)
        .expect("a complete mainnet capture with valid integrity digests is required");
    assert_eq!(captured.provenance().protocol_version(), 30);
    let env = soroban_sdk::Env::default();
    let entries = &captured.frozen_fixture().ledger_snapshot().ledger_entries;
    // Pool/aggregator pins come from deployments/mainnet-v3-yieldblent-pool.json.
    // Reflector is upgradeable: check its captured bytes against its own instance.
    for (contract, expected) in [
        (
            POOL,
            Some("0e4f8f45b94e6b0d017a496ac5620ad0e3f866533cf45acd2b070bedda1e7356"),
        ),
        (
            ORACLE,
            Some("41df04894bf2d8291492326f7a6ab9c90a90cc9828e31990ce8aeeab42680bfd"),
        ),
        (REFLECTOR, None),
    ] {
        let address = xdr::ScAddress::from(&Address::from_str(&env, contract));
        let hash = entries
            .iter()
            .find_map(|(_, (entry, _))| {
                let xdr::LedgerEntryData::ContractData(data) = &entry.data else {
                    return None;
                };
                if data.contract != address || data.key != xdr::ScVal::LedgerKeyContractInstance {
                    return None;
                }
                let xdr::ScVal::ContractInstance(instance) = &data.val else {
                    return None;
                };
                let xdr::ContractExecutable::Wasm(hash) = &instance.executable else {
                    return None;
                };
                Some(hash)
            })
            .expect("deployed instance must be captured");
        if let Some(expected) = expected {
            assert_eq!(hex(&hash.0), expected, "unexpected deployment: {contract}");
        }
        let code = entries
            .iter()
            .find_map(|(_, (entry, _))| {
                let xdr::LedgerEntryData::ContractCode(code) = &entry.data else {
                    return None;
                };
                (code.hash == *hash).then_some(code)
            })
            .expect("deployed WASM must be captured");
        assert_eq!(Sha256::digest(code.code.as_slice()).as_slice(), hash.0);
    }
    captured
}

// Compare state/TTLs while ignoring code bytes and only the pool executable pin.
fn storage_digest(fork: &ScenarioFork<'_>) -> [u8; 32] {
    let address = xdr::ScAddress::from(&fork.contract(POOL));
    let mut snapshot = fork.env().to_ledger_snapshot();
    snapshot
        .ledger_entries
        .retain(|(key, _)| !matches!(key.as_ref(), xdr::LedgerKey::ContractCode(_)));
    for (_, (entry, _)) in &mut snapshot.ledger_entries {
        if let xdr::LedgerEntryData::ContractData(data) = &mut entry.data {
            if data.contract == address && data.key == xdr::ScVal::LedgerKeyContractInstance {
                let xdr::ScVal::ContractInstance(instance) = &mut data.val else {
                    unreachable!()
                };
                instance.executable = xdr::ContractExecutable::Wasm(xdr::Hash([0; 32]));
            }
        }
    }
    canonical_ledger_digest(&snapshot).unwrap()
}

#[derive(Debug, PartialEq)]
struct Outcome {
    oracle_round: u64,
    debt_before: i128,
    debt_after: i128,
    final_storage: [u8; 32],
}

fn scenario(fork: &ScenarioFork<'_>, candidate: bool) -> Outcome {
    fork.mock_all_auths();
    assert_eq!(fork.network_protocol_version(), 30);
    assert_eq!(fork.executed_protocol_version(), 30);
    let env = fork.env();
    let address = fork.contract(POOL);
    if candidate {
        let before = storage_digest(fork);
        let installed = fork.replace_wasm(&address, pool::WASM);
        assert_eq!(installed.as_slice(), Sha256::digest(pool::WASM).as_slice());
        assert_eq!(storage_digest(fork), before);
    }
    let client = pool::Client::new(env, &address);
    let oracle = fork.contract(ORACLE);
    assert_eq!(client.get_config().oracle, oracle);
    let xlm = fork.contract(XLM);
    let usdc = fork.contract(USDC);
    let borrower = fork.local_account("blnt-oracle-outage-borrower-v1");
    // Only the borrower/funding is synthetic. Prices and reserves stay captured.
    fork.fund_local_account(&borrower, 1000_0000000);
    let xdr::ScAddress::Account(account) = xdr::ScAddress::from(&borrower) else {
        unreachable!()
    };
    // Local mocked source-account authorization; no keys or signatures.
    env.host().set_source_account(account).unwrap();
    fork.invoke::<()>(&usdc, "trust", (&borrower,));
    let balance = |token: &Address, who: &Address| fork.invoke::<i128>(token, "balance", (who,));
    let price =
        || fork.invoke::<Option<PriceData>>(&oracle, "lastprice", (Asset::Stellar(xlm.clone()),));
    let first_price = price().expect("captured XLM price must be available");
    let max_age: u64 = fork.invoke(&oracle, "max_age", ());
    assert_eq!(max_age, 900);
    let request = |kind, asset: &Address, amount| pool::Request {
        request_type: kind,
        address: asset.clone(),
        amount,
    };
    let submit = |requests: &soroban_sdk::Vec<pool::Request>| {
        client.submit(&borrower, &borrower, &borrower, requests)
    };
    let start = submit(&vec![
        env,
        request(SUPPLY_COLLATERAL, &xlm, 100_0000000),
        request(BORROW, &usdc, 1_0000000),
    ]);
    assert_eq!(start.liabilities.len(), 1);
    assert_eq!(balance(&usdc, &borrower), 1_0000000);
    let boundary = first_price.timestamp.checked_add(max_age).unwrap();
    assert!(
        boundary >= env.ledger().timestamp(),
        "capture must start with a fresh price"
    );
    // No new oracle rounds arrive during this local time jump.
    env.ledger().set_timestamp(boundary);
    assert_eq!(price(), Some(first_price.clone()));
    submit(&vec![env, request(BORROW, &usdc, EXTRA_BORROW)]);
    assert_eq!(balance(&usdc, &borrower), 1_0100000);
    env.ledger().set_timestamp(boundary.checked_add(1).unwrap());
    assert_eq!(price(), None);
    let base_price: Option<PriceData> =
        fork.invoke(&oracle, "lastprice", (Asset::Stellar(usdc.clone()),));
    assert_eq!(
        base_price,
        Some(PriceData {
            price: 1_0000000,
            timestamp: env.ledger().timestamp()
        })
    );
    for rejected in [
        request(BORROW, &usdc, EXTRA_BORROW),
        request(WITHDRAW_COLLATERAL, &xlm, 1_0000000),
    ] {
        let before = canonical_ledger_digest(&env.to_ledger_snapshot()).unwrap();
        let failure = fork.try_invoke::<pool::Positions>(
            &address,
            "submit",
            (&borrower, &borrower, &borrower, vec![env, rejected]),
        );
        let failure =
            failure.expect_err("risk-increasing action must fail without collateral price");
        assert_eq!(failure.kind, InvokeErrorKind::Abort);
        assert_eq!(
            canonical_ledger_digest(&env.to_ledger_snapshot()).unwrap(),
            before
        );
        println!("outage rejection: {:?}", failure.kind);
    }
    let before = client.get_positions(&borrower);
    let reserve = client.get_reserve(&usdc);
    let debt_before = before.liabilities.get(reserve.config.index).unwrap();
    let wallet_before = balance(&usdc, &borrower);
    let custody_before = balance(&usdc, &address);
    let repaid = submit(&vec![env, request(REPAY, &usdc, PARTIAL_REPAY)]);
    let debt_after = repaid.liabilities.get(reserve.config.index).unwrap();
    assert!(debt_after > 0 && debt_after < debt_before);
    assert_eq!(
        debt_before - debt_after,
        PARTIAL_REPAY * 1_000_000_000_000 / reserve.data.d_rate
    );
    assert_eq!(balance(&usdc, &borrower), wallet_before - PARTIAL_REPAY);
    assert_eq!(balance(&usdc, &address), custody_before + PARTIAL_REPAY);
    assert_eq!(
        reserve.data.d_supply - client.get_reserve(&usdc).data.d_supply,
        debt_before - debt_after
    );
    assert_eq!(repaid.collateral, before.collateral);
    let xlm_before = balance(&xlm, &borrower);
    let xlm_custody_before = balance(&xlm, &address);
    let xlm_reserve = client.get_reserve(&xlm);
    let more_collateral = submit(&vec![env, request(SUPPLY_COLLATERAL, &xlm, 10_0000000)]);
    assert_eq!(more_collateral.liabilities, repaid.liabilities);
    let xlm_index = xlm_reserve.config.index;
    assert_eq!(
        more_collateral.collateral.get(xlm_index).unwrap()
            - repaid.collateral.get(xlm_index).unwrap(),
        10_0000000 * 1_000_000_000_000 / xlm_reserve.data.b_rate
    );
    assert_eq!(balance(&xlm, &borrower), xlm_before - 10_0000000);
    assert_eq!(balance(&xlm, &address), xlm_custody_before + 10_0000000);
    assert_eq!(price(), None);
    println!("candidate={candidate} oracle_round={} boundary={boundary} debt_shares={debt_before}->{debt_after} partial_repay=5000000 added_collateral=100000000", first_price.timestamp);
    Outcome {
        oracle_round: first_price.timestamp,
        debt_before,
        debt_after,
        final_storage: storage_digest(fork),
    }
}

fn replay(path: &Path) {
    let mut outcomes = Vec::new();
    for candidate in [false, true] {
        let outcome = RefCell::new(None);
        let result = mainnet()
            .cache(path)
            .offline()
            .run(|fork| {
                *outcome.borrow_mut() = Some(scenario(fork, candidate));
            })
            .expect("offline scenario");
        assert_eq!(result.fixture().report().final_replay_rpc_reads(), 0);
        outcomes.push(outcome.into_inner().unwrap());
    }
    assert_eq!(outcomes[0], outcomes[1]);
    println!(
        "deployed/source state matches; source_sha256={}; rpc_reads=0",
        hex(&Sha256::digest(pool::WASM))
    );
}

#[test]
fn oracle_outage_preserves_risk_reducing_actions() {
    let path = std::env::var_os("BLNT_MAINNET_FIXTURE")
        .expect("run make test-mainnet to select the local snapshot");
    let bytes = std::fs::read(&path).expect("run make prepare-mainnet-fixture before testing");
    let captured = validate_capture(Path::new(&path));
    println!(
        "ledger={} ledger_hash={} fixture_sha256={}",
        captured.provenance().ledger_sequence(),
        captured.provenance().ledger_hash(),
        hex(&Sha256::digest(bytes))
    );
    replay(Path::new(&path));
}

#[test]
#[ignore = "explicit read-only RPC capture; BLNT_CAPTURE_OUTPUT must name a new file"]
fn capture_fixture() {
    let path = std::env::var_os("BLNT_CAPTURE_OUTPUT").expect("set BLNT_CAPTURE_OUTPUT");
    assert!(
        !Path::new(&path).exists(),
        "choose a new path; existing snapshots are never replaced"
    );
    mainnet()
        .cache(&path)
        .refresh()
        .run(|fork| {
            scenario(fork, false);
        })
        .expect("capture");
    let captured = validate_capture(Path::new(&path));
    replay(Path::new(&path));
    println!(
        "captured ledger={} ledger_hash={}",
        captured.provenance().ledger_sequence(),
        captured.provenance().ledger_hash()
    );
}
