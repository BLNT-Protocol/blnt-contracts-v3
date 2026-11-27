mod support;
use kanatoko::multihost::{replay, Session};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use support::{pool, Market, Operation::*, PriceData, PINS, POOL, RATE_SCALE, UNIT, USDC, XLM};

fn oracle_outage(fork: &dyn Session) -> (u64, i128, i128, [u8; 32]) {
    let pool = Market::new(fork);
    let price = pool
        .price(XLM)
        .expect("capture starts with a fresh XLM price");
    let opened = pool
        .submit(&[(Supply, XLM, 100 * UNIT), (Borrow, USDC, UNIT)])
        .unwrap();
    assert_eq!(opened.liabilities.len(), 1);
    assert_eq!(pool.wallet(USDC), UNIT);

    assert_eq!(pool.max_age(), 900);
    let boundary = price.timestamp.checked_add(900).unwrap();
    assert!(boundary >= fork.timestamp());
    fork.set_timestamp(boundary);
    assert_eq!(pool.price(XLM), Some(price.clone()));
    pool.submit(&[(Borrow, USDC, UNIT / 100)]).unwrap();
    assert_eq!(pool.wallet(USDC), UNIT + UNIT / 100);

    fork.set_timestamp(boundary + 1);
    assert_eq!(pool.price(XLM), None);
    assert_eq!(
        pool.price(USDC),
        Some(PriceData {
            price: UNIT,
            timestamp: boundary + 1
        })
    );
    for rejected in [(Borrow, USDC, UNIT / 100), (Withdraw, XLM, UNIT)] {
        let before = fork.digest();
        assert_eq!(pool.submit(&[rejected]).unwrap_err(), "Abort");
        assert_eq!(fork.digest(), before);
    }

    let (repaid, debt_before, debt_after) = assert_partial_repayment(&pool);
    assert_collateral_top_up(&pool, &repaid);
    (
        price.timestamp,
        debt_before,
        debt_after,
        fork.storage_digest(POOL),
    )
}

fn assert_partial_repayment(pool: &Market<'_>) -> (pool::Positions, i128, i128) {
    let before = pool.positions();
    let reserve = pool.reserve(USDC);
    let index = reserve.config.index;
    let debt_before = before.liabilities.get(index).unwrap();
    let (wallet, custody) = (pool.wallet(USDC), pool.custody(USDC));
    let repayment = UNIT / 2;
    let repaid = pool.submit(&[(Repay, USDC, repayment)]).unwrap();
    let debt_after = repaid.liabilities.get(index).unwrap();
    assert!(debt_after > 0 && debt_after < debt_before);
    assert_eq!(
        debt_before - debt_after,
        repayment * RATE_SCALE / reserve.data.d_rate
    );
    assert_eq!(pool.wallet(USDC), wallet - repayment);
    assert_eq!(pool.custody(USDC), custody + repayment);
    assert_eq!(
        reserve.data.d_supply - pool.reserve(USDC).data.d_supply,
        debt_before - debt_after
    );
    assert_eq!(repaid.collateral, before.collateral);
    (repaid, debt_before, debt_after)
}

fn assert_collateral_top_up(pool: &Market<'_>, repaid: &pool::Positions) {
    let xlm = pool.reserve(XLM);
    let index = xlm.config.index;
    let (wallet, custody) = (pool.wallet(XLM), pool.custody(XLM));
    let top_up = 10 * UNIT;
    let topped_up = pool.submit(&[(Supply, XLM, top_up)]).unwrap();
    assert_eq!(topped_up.liabilities, repaid.liabilities);
    assert_eq!(
        topped_up.collateral.get(index).unwrap() - repaid.collateral.get(index).unwrap(),
        top_up * RATE_SCALE / xlm.data.b_rate
    );
    assert_eq!(pool.wallet(XLM), wallet - top_up);
    assert_eq!(pool.custody(XLM), custody + top_up);
    assert_eq!(pool.price(XLM), None);
}

fn compare_pools(path: &Path) {
    let (protocol, deployed) = replay(path, PINS, |fork| {
        (fork.protocol_version(), oracle_outage(fork))
    });
    let candidate = replay(path, PINS, |fork| {
        let before = fork.storage_digest(POOL);
        let installed = fork.replace_wasm(POOL, pool::WASM);
        assert_eq!(installed, <[u8; 32]>::from(Sha256::digest(pool::WASM)));
        assert_eq!(fork.storage_digest(POOL), before);
        oracle_outage(fork)
    });
    assert_eq!(deployed, candidate);
    println!(
        "P{protocol} round={} debt_shares={}->{} final_storage={:02x?}",
        deployed.0, deployed.1, deployed.2, deployed.3
    );
}

#[test]
fn oracle_outage_preserves_risk_reducing_actions() {
    let path = std::env::var_os("BLNT_MAINNET_FIXTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let cache = std::env::var_os("XDG_CACHE_HOME")
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
                .expect("set BLNT_MAINNET_FIXTURE to the local snapshot");
            cache.join("blnt-contracts-v3/oracle-outage-p30-v1.json")
        });
    assert!(
        path.is_file(),
        "run make prepare-mainnet-fixture first: {}",
        path.display()
    );
    compare_pools(&path);
}

#[test]
#[ignore = "explicit read-only RPC capture; BLNT_CAPTURE_OUTPUT must name a new file"]
fn capture_fixture() {
    let path =
        PathBuf::from(std::env::var_os("BLNT_CAPTURE_OUTPUT").expect("set BLNT_CAPTURE_OUTPUT"));
    assert!(
        !path.exists(),
        "choose a new path to preserve the existing snapshot"
    );
    let captured = kanatoko::mainnet()
        .cache(&path)
        .refresh()
        .run(|fork| {
            oracle_outage(fork);
        })
        .expect("capture");
    compare_pools(&path);
    println!(
        "captured ledger {}",
        captured.fixture().provenance().ledger_sequence()
    );
}
