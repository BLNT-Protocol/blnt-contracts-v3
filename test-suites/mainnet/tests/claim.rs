use std::{cell::RefCell, path::Path};

use kanatoko::{
    canonical_ledger_digest, mainnet,
    soroban_sdk::{vec, xdr, Address, IntoVal, TryFromVal, Val},
    CapturedFixture, InvokeErrorKind, PreviewAuth, ScenarioFork,
};
use sha2::{Digest, Sha256};

// Generated bindings retain the protocol's existing public API signatures.
#[allow(clippy::too_many_arguments)]
mod backstop {
    use kanatoko::soroban_sdk;
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/optimized/backstop.wasm");
}

const BACKSTOP: &str = "CAHCUKBPTW5DR5RNXOHCTJD5U76A6EEZXYZAQFRQEEQRXQ3CRUZ6Z3MO";
const POOL: &str = "CAUTPSQDAJEUV5XEU3EXFVYRGDJLEJGVTFJCFZCSPTDCREAMZ573VF4D";
const OWNER: &str = "GDJSH2NU2WF6J4P5DL4522DUCABWSTZOKFQ7BHBCFYQ3QKC6FRYWP6OL";
const COMET: &str = "CA5HX3D47PQZWC2E7VWXCKH4534P2NOJEOIXAWU47MI4QV2FQUYY3FLC";
const BLNT: &str = "CCBNWP5L7ZSXA3DBWCMFAVKFVRW3ZHWO7B5J557W42O64DDZNPJP7VDS";
const XLM: &str = "CAS3J7GYLGXMF6TDJBBYYSE3HQ6BBSMLNUQ34T6TZMYMW2EVH34XOWMA";
const NETWORK: &str = "Public Global Stellar Network ; September 2015";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

// Pin the deployed system, not one contributor's capture of a particular ledger.
fn validate_capture(path: &Path) -> CapturedFixture {
    let captured = CapturedFixture::from_file(path, NETWORK)
        .expect("a complete mainnet capture with valid integrity digests is required");
    assert_eq!(captured.provenance().protocol_version(), 29);
    let env = kanatoko::soroban_sdk::Env::default();
    let entries = &captured.frozen_fixture().ledger_snapshot().ledger_entries;
    for (contract, expected) in [
        (
            BACKSTOP,
            "0469cfb445bedd87478274f265c8776b32cde1e8840fca4684ffa1dc4d5a750f",
        ),
        (
            COMET,
            "d735c3395f59510172cf5cf838823a1389b97cfec6bf24e580e9bd77d2b3e687",
        ),
        (
            "CDQMIKIHKA5UY3FQJ2UDTLNX3GOPD3GPNZWSPTOBVOHEUMQJQXOPC7Y3",
            "d735c3395f59510172cf5cf838823a1389b97cfec6bf24e580e9bd77d2b3e687",
        ),
        (
            "CDSO6FQSBJ3U3AJHBNNLHHKSMKTTDDN3GLGF3KZTMXBFV7SVB43ZJERT",
            "4fa7d99dee6fe99cad5d07a46ee39e437dc287e53e3ab1f3ccc0aacf7e6cd6c5",
        ),
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
            .expect("canonical deployed instance must be captured");
        assert_eq!(hex(&hash.0), expected, "unexpected deployment: {contract}");
        let code = entries
            .iter()
            .find_map(|(_, (entry, _))| {
                let xdr::LedgerEntryData::ContractCode(code) = &entry.data else {
                    return None;
                };
                (code.hash == *hash).then_some(code)
            })
            .expect("deployed WASM must be captured");
        assert_eq!(hex(&Sha256::digest(code.code.as_slice())), expected);
    }
    captured
}

// Exclude code entries and normalize only the backstop's executable reference.
// Equality then proves that local replacement preserves all captured state/TTLs.
fn storage_digest(fork: &ScenarioFork<'_>) -> [u8; 32] {
    let address = xdr::ScAddress::from(&fork.contract(BACKSTOP));
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

fn source_account(fork: &ScenarioFork<'_>, address: &Address) {
    let xdr::ScAddress::Account(account) = xdr::ScAddress::from(address) else {
        panic!("expected a G-account");
    };
    // Local mocked source-account authorization; no keys or signatures.
    fork.env().host().set_source_account(account).unwrap();
}

fn detached(fork: &ScenarioFork<'_>, value: Val) -> xdr::ScVal {
    xdr::ScVal::try_from_val(fork.env(), &value).unwrap()
}

fn balance(fork: &ScenarioFork<'_>, token: &str, owner: &Address) -> i128 {
    fork.invoke(&fork.contract(token), "balance", (owner,))
}

#[derive(Debug, PartialEq)]
struct Position {
    reward: i128,
    lp: i128,
    user: xdr::ScVal,
    other_tier: xdr::ScVal,
    pool: xdr::ScVal,
}

fn position(fork: &ScenarioFork<'_>) -> Position {
    let address = fork.contract(BACKSTOP);
    let owner = fork.account(OWNER);
    let pool = fork.contract(POOL);
    Position {
        reward: balance(fork, BLNT, &address),
        lp: balance(fork, COMET, &address),
        user: detached(
            fork,
            fork.invoke(
                &address,
                "user_balance",
                (backstop::BackstopTier::FirstLoss, &pool, &owner),
            ),
        ),
        other_tier: detached(
            fork,
            fork.invoke(
                &address,
                "user_balance",
                (backstop::BackstopTier::SecondLoss, &pool, &owner),
            ),
        ),
        pool: detached(fork, fork.invoke(&address, "pool_data", (&pool,))),
    }
}

#[derive(Debug, PartialEq)]
struct Outcome {
    reward: i128,
    before: i128,
    after: i128,
    shares: i128,
    trade_blnt: i128,
    trade_xlm: i128,
    final_position: Position,
}

fn scenario(fork: &ScenarioFork<'_>, candidate: bool) -> Outcome {
    fork.mock_all_auths();
    assert_eq!(fork.network_protocol_version(), 29);
    assert_eq!(fork.executed_protocol_version(), 29);
    let env = fork.env();
    let address = fork.contract(BACKSTOP);
    let owner = fork.account(OWNER);
    let pool = fork.contract(POOL);
    let blnt = fork.contract(BLNT);
    let comet = fork.contract(COMET);
    let xlm = fork.contract(XLM);
    let initial = position(fork);
    if candidate {
        let before = storage_digest(fork);
        let installed = fork.replace_wasm(&address, backstop::WASM);
        assert_eq!(
            installed.as_slice(),
            Sha256::digest(backstop::WASM).as_slice()
        );
        assert_eq!(storage_digest(fork), before);
        assert_eq!(position(fork), initial);
    }

    // Only the trader is synthetic. No protocol position or reserve is edited.
    let trader = fork.local_account("blnt-core-claim-trader-v1");
    fork.fund_local_account(&trader, 10_0000000);
    source_account(fork, &trader);
    fork.invoke::<()>(&blnt, "trust", (&trader,));
    let reserve: i128 = fork.invoke(&comet, "get_balance", (&blnt,));
    let input = reserve / 100;
    let admin: Address = fork.invoke(&blnt, "admin", ());
    // Explicit local token-admin context, not real administrative authorization.
    env.as_contract(&admin, || {
        fork.invoke::<()>(&blnt, "mint", (&trader, input))
    });
    assert_eq!(position(fork), initial);

    let client = backstop::Client::new(env, &address);
    let tier = backstop::BackstopTier::FirstLoss;
    let pools = vec![env, pool.clone()];
    let preview = || {
        fork.preview(
            &address,
            "claim",
            (&tier, &owner, &pools, 0i128),
            PreviewAuth::Record,
        )
        .expect("claim footprint must be captured")
        .result::<i128>(env)
        .expect("claim preview must succeed")
    };
    source_account(fork, &owner);
    let before = preview();
    let old_minimum = before * 995 / 1000;
    assert!(old_minimum > 0);

    source_account(fork, &trader);
    fork.invoke::<()>(
        &blnt,
        "approve",
        (&trader, &comet, input, env.ledger().sequence() + 100),
    );
    let spot: i128 = fork.invoke(&comet, "get_spot_price", (&blnt, &xlm));
    let xlm_before = balance(fork, XLM, &trader);
    let (output, _): (i128, i128) = fork.invoke(
        &comet,
        "swap_exact_amount_in",
        (&blnt, input, &xlm, 1i128, spot * 2, &trader),
    );
    assert!(output > 0);
    assert_eq!(balance(fork, BLNT, &trader), 0);
    assert_eq!(balance(fork, XLM, &trader) - xlm_before, output);
    assert_eq!(
        fork.invoke::<i128>(&comet, "get_balance", (&blnt,)),
        reserve + input
    );

    source_account(fork, &owner);
    let after = preview();
    assert!(after > 0 && after < old_minimum);
    let post_trade = position(fork);
    let digest = canonical_ledger_digest(&env.to_ledger_snapshot()).unwrap();
    let failure = fork
        .try_invoke::<i128>(&address, "claim", (&tier, &owner, &pools, old_minimum))
        .expect_err("the stale minimum must reject the claim");
    assert_eq!(failure.kind, InvokeErrorKind::Contract(20));
    assert_eq!(
        canonical_ledger_digest(&env.to_ledger_snapshot()).unwrap(),
        digest
    );
    assert_eq!(position(fork), post_trade);

    let user_before = client.user_balance(&tier, &pool, &owner);
    let tier_before = client.pool_data(&pool).tiers.get(0).unwrap();
    let new_minimum = after * 995 / 1000;
    assert!(new_minimum > 0);
    let received = client.claim(&tier, &owner, &pools, &new_minimum);
    assert_eq!(received, after);
    let final_position = position(fork);
    let reward = post_trade.reward - final_position.reward;
    assert!(reward > 0);
    assert_eq!(final_position.lp - post_trade.lp, received);
    assert_eq!(
        fork.invoke::<i128>(&comet, "get_balance", (&blnt,)),
        reserve + input + reward
    );
    assert_eq!(final_position.other_tier, post_trade.other_tier);
    let user_after = client.user_balance(&tier, &pool, &owner);
    let shares = received * tier_before.shares / tier_before.tokens;
    assert_eq!(user_after.shares - user_before.shares, shares);
    assert_eq!(
        detached(fork, user_after.q4w.into_val(env)),
        detached(fork, user_before.q4w.into_val(env))
    );
    let tier_after = client.pool_data(&pool).tiers.get(0).unwrap();
    assert_eq!(tier_after.tokens - tier_before.tokens, received);
    assert_eq!(tier_after.shares - tier_before.shares, shares);
    assert_eq!(client.claim(&tier, &owner, &pools, &new_minimum), 0);
    assert_eq!(position(fork), final_position);
    Outcome {
        reward,
        before,
        after,
        shares,
        trade_blnt: input,
        trade_xlm: output,
        final_position,
    }
}

fn replay(path: &Path, candidate: bool) -> Outcome {
    let result = RefCell::new(None);
    let run = mainnet()
        .cache(path)
        .offline()
        .run(|fork| {
            *result.borrow_mut() = Some(scenario(fork, candidate));
        })
        .expect("replay requires the complete saved fixture and no RPC");
    assert_eq!(run.fixture().report().final_replay_rpc_reads(), 0);
    result.into_inner().unwrap()
}

#[test]
fn source_backstop_matches_mainnet_claim_after_a_market_move() {
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
    let deployed = replay(Path::new(&path), false);
    println!(
        "deployed: reward={} before={} after={}",
        deployed.reward, deployed.before, deployed.after
    );
    let candidate = replay(Path::new(&path), true);
    assert_eq!(candidate, deployed);
    println!("source: reward={} before={} after={} shares={} sold_blnt={} received_xlm={}; code_sha256={}; rpc_reads=0", candidate.reward, candidate.before, candidate.after, candidate.shares, candidate.trade_blnt, candidate.trade_xlm, hex(&Sha256::digest(backstop::WASM)));
}

#[test]
#[ignore = "explicit read-only RPC capture; BLNT_CAPTURE_OUTPUT must name a new file"]
fn capture_fixture() {
    let path = std::env::var_os("BLNT_CAPTURE_OUTPUT").expect("set BLNT_CAPTURE_OUTPUT");
    assert!(
        !Path::new(&path).exists(),
        "choose a new output path; existing snapshots are never replaced"
    );
    mainnet()
        .cache(&path)
        .refresh()
        .run(|fork| {
            scenario(fork, false);
        })
        .expect("capture and strict replay must succeed");
    let captured = validate_capture(Path::new(&path));
    let deployed = replay(Path::new(&path), false);
    let candidate = replay(Path::new(&path), true);
    assert_eq!(candidate, deployed);
    println!(
        "captured ledger={} ledger_hash={} reward={} before={} after={} shares={}",
        captured.provenance().ledger_sequence(),
        captured.provenance().ledger_hash(),
        candidate.reward,
        candidate.before,
        candidate.after,
        candidate.shares
    );
}
