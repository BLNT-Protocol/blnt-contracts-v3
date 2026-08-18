#![cfg(test)]

use soroban_sdk::{
    testutils::{Address as _, BytesN as _, Events},
    vec, Address, BytesN, Env, IntoVal, String, Symbol,
};

use crate::{
    pool_factory::validate_backstop_config, BackstopAsset, BackstopTierConfig, PoolFactoryClient,
    PoolFactoryContract, PoolInitMeta,
};

fn backstop_config(e: &Env) -> soroban_sdk::Vec<BackstopTierConfig> {
    vec![
        e,
        BackstopTierConfig {
            asset: BackstopAsset::BlndXlm,
            take_rate_weight: 1,
        },
    ]
}

mod pool {
    soroban_sdk::contractimport!(file = "../target/wasm32v1-none/optimized/pool.wasm");
}

#[test]
fn test_pool_factory() {
    let e = Env::default();
    e.cost_estimate().budget().reset_unlimited();
    e.mock_all_auths_allowing_non_root_auth();

    let wasm_hash = e.deployer().upload_contract_wasm(pool::WASM);

    let bombadil = Address::generate(&e);

    let oracle = Address::generate(&e);
    let backstop_id = Address::generate(&e);
    let backstop_rate: u32 = 0_1000000;
    let max_positions: u32 = 6;
    let min_collateral: i128 = 1_0000000;
    let blnd_id = Address::generate(&e);

    let pool_init_meta = PoolInitMeta {
        backstop: backstop_id.clone(),
        pool_hash: wasm_hash.clone(),
        blnd_id: blnd_id.clone(),
    };
    let pool_factory_address = e.register(PoolFactoryContract {}, (pool_init_meta,));
    let pool_factory_client = PoolFactoryClient::new(&e, &pool_factory_address);

    let name1 = String::from_str(&e, "pool1");
    let name2 = String::from_str(&e, "pool2");
    let salt = BytesN::<32>::random(&e);

    let config = backstop_config(&e);
    let deployed_pool_address_1 = pool_factory_client.deploy(
        &bombadil,
        &name1,
        &salt,
        &oracle,
        &backstop_rate,
        &max_positions,
        &min_collateral,
        &config,
    );

    let event = e.events().all().filter_by_contract(&pool_factory_address);
    assert_eq!(
        event,
        vec![
            &e,
            (
                pool_factory_address.clone(),
                (Symbol::new(&e, "deploy"),).into_val(&e),
                deployed_pool_address_1.to_val()
            )
        ]
    );

    let salt = BytesN::<32>::random(&e);
    let deployed_pool_address_2 = pool_factory_client.deploy(
        &bombadil,
        &name2,
        &salt,
        &oracle,
        &backstop_rate,
        &max_positions,
        &min_collateral,
        &backstop_config(&e),
    );

    e.as_contract(&deployed_pool_address_1, || {
        assert_eq!(
            e.storage()
                .instance()
                .get::<_, Address>(&Symbol::new(&e, "Admin"))
                .unwrap(),
            bombadil.clone()
        );
        assert_eq!(
            e.storage()
                .instance()
                .get::<_, Address>(&Symbol::new(&e, "Backstop"))
                .unwrap(),
            backstop_id.clone()
        );
        assert_eq!(
            e.storage()
                .instance()
                .get::<_, pool::PoolConfig>(&Symbol::new(&e, "Config"))
                .unwrap(),
            pool::PoolConfig {
                oracle: oracle,
                min_collateral: min_collateral,
                bstop_rate: backstop_rate,
                status: 6,
                max_positions: 6
            }
        );
        assert_eq!(
            e.storage()
                .instance()
                .get::<_, Address>(&Symbol::new(&e, "BLNDTkn"))
                .unwrap(),
            blnd_id.clone()
        );
    });
    assert_ne!(deployed_pool_address_1, deployed_pool_address_2);
    assert!(pool_factory_client.is_pool(&deployed_pool_address_1));
    assert!(pool_factory_client.is_pool(&deployed_pool_address_2));
    assert!(!pool_factory_client.is_pool(&Address::generate(&e)));
    assert_eq!(
        pool_factory_client.backstop_config(&deployed_pool_address_1),
        config
    );
}

#[test]
fn test_pool_factory_accepts_three_ordered_backstop_tiers() {
    let e = Env::default();
    let config = vec![
        &e,
        BackstopTierConfig {
            asset: BackstopAsset::BlndXlm,
            take_rate_weight: 4,
        },
        BackstopTierConfig {
            asset: BackstopAsset::BlndUsdc,
            take_rate_weight: 3,
        },
        BackstopTierConfig {
            asset: BackstopAsset::Usdc,
            take_rate_weight: 2,
        },
    ];
    validate_backstop_config(&e, &config);
}

#[test]
fn test_pool_factory_accepts_maximum_backstop_weight() {
    let e = Env::default();
    validate_backstop_config(
        &e,
        &vec![
            &e,
            BackstopTierConfig {
                asset: BackstopAsset::BlndXlm,
                take_rate_weight: 10,
            },
        ],
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #1300)")]
fn test_pool_factory_rejects_empty_backstop_config() {
    let e = Env::default();
    validate_backstop_config(&e, &soroban_sdk::Vec::new(&e));
}

#[test]
#[should_panic(expected = "Error(Contract, #1300)")]
fn test_pool_factory_rejects_duplicate_backstop_tokens() {
    let e = Env::default();
    validate_backstop_config(
        &e,
        &vec![
            &e,
            BackstopTierConfig {
                asset: BackstopAsset::Xlm,
                take_rate_weight: 1,
            },
            BackstopTierConfig {
                asset: BackstopAsset::Xlm,
                take_rate_weight: 2,
            },
        ],
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #1300)")]
fn test_pool_factory_rejects_zero_backstop_weight() {
    let e = Env::default();
    validate_backstop_config(
        &e,
        &vec![
            &e,
            BackstopTierConfig {
                asset: BackstopAsset::Usdc,
                take_rate_weight: 0,
            },
        ],
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #1300)")]
fn test_pool_factory_rejects_more_than_three_backstop_tiers() {
    let e = Env::default();
    validate_backstop_config(
        &e,
        &vec![
            &e,
            BackstopTierConfig {
                asset: BackstopAsset::BlndXlm,
                take_rate_weight: 1,
            },
            BackstopTierConfig {
                asset: BackstopAsset::BlndUsdc,
                take_rate_weight: 1,
            },
            BackstopTierConfig {
                asset: BackstopAsset::Usdc,
                take_rate_weight: 1,
            },
            BackstopTierConfig {
                asset: BackstopAsset::Xlm,
                take_rate_weight: 1,
            },
        ],
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #1300)")]
fn test_pool_factory_rejects_excessive_backstop_weight() {
    let e = Env::default();
    validate_backstop_config(
        &e,
        &vec![
            &e,
            BackstopTierConfig {
                asset: BackstopAsset::Usdc,
                take_rate_weight: 11,
            },
        ],
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #1300)")]
fn test_pool_factory_rejects_equal_backstop_weights() {
    let e = Env::default();
    validate_backstop_config(
        &e,
        &vec![
            &e,
            BackstopTierConfig {
                asset: BackstopAsset::BlndXlm,
                take_rate_weight: 4,
            },
            BackstopTierConfig {
                asset: BackstopAsset::Usdc,
                take_rate_weight: 4,
            },
        ],
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #1300)")]
fn test_pool_factory_rejects_ascending_backstop_weights() {
    let e = Env::default();
    validate_backstop_config(
        &e,
        &vec![
            &e,
            BackstopTierConfig {
                asset: BackstopAsset::BlndXlm,
                take_rate_weight: 3,
            },
            BackstopTierConfig {
                asset: BackstopAsset::Usdc,
                take_rate_weight: 4,
            },
        ],
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #1300)")]
fn test_pool_factory_invalid_pool_init_args_backstop_rate() {
    let e = Env::default();
    e.cost_estimate().budget().reset_unlimited();
    e.mock_all_auths_allowing_non_root_auth();

    let wasm_hash = e.deployer().upload_contract_wasm(pool::WASM);

    let backstop_id = Address::generate(&e);
    let blnd_id = Address::generate(&e);

    let pool_init_meta = PoolInitMeta {
        backstop: backstop_id.clone(),
        pool_hash: wasm_hash.clone(),
        blnd_id: blnd_id.clone(),
    };
    let pool_factory_address = e.register(PoolFactoryContract {}, (pool_init_meta,));
    let pool_factory_client = PoolFactoryClient::new(&e, &pool_factory_address);

    let bombadil = Address::generate(&e);
    let oracle = Address::generate(&e);
    let backstop_rate: u32 = 1_0000000;
    let max_positions: u32 = 6;
    let min_collateral: i128 = 1_0000000;
    let name1 = String::from_str(&e, "pool1");
    let salt = BytesN::<32>::random(&e);

    pool_factory_client.deploy(
        &bombadil,
        &name1,
        &salt,
        &oracle,
        &backstop_rate,
        &max_positions,
        &min_collateral,
        &backstop_config(&e),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #1300)")]
fn test_pool_factory_invalid_pool_init_args_max_positions() {
    let e = Env::default();
    e.cost_estimate().budget().reset_unlimited();
    e.mock_all_auths_allowing_non_root_auth();
    let wasm_hash = e.deployer().upload_contract_wasm(pool::WASM);

    let backstop_id = Address::generate(&e);
    let blnd_id = Address::generate(&e);

    let pool_init_meta = PoolInitMeta {
        backstop: backstop_id.clone(),
        pool_hash: wasm_hash.clone(),
        blnd_id: blnd_id.clone(),
    };
    let pool_factory_address = e.register(PoolFactoryContract {}, (pool_init_meta,));
    let pool_factory_client = PoolFactoryClient::new(&e, &pool_factory_address);

    let bombadil = Address::generate(&e);
    let oracle = Address::generate(&e);
    let backstop_rate: u32 = 0_1000000;
    let max_positions: u32 = 1;
    let min_collateral: i128 = 1_0000000;

    let name1 = String::from_str(&e, "pool1");
    let salt = BytesN::<32>::random(&e);

    pool_factory_client.deploy(
        &bombadil,
        &name1,
        &salt,
        &oracle,
        &backstop_rate,
        &max_positions,
        &min_collateral,
        &backstop_config(&e),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #1300)")]
fn test_pool_factory_invalid_pool_init_args_max_positions_large() {
    let e = Env::default();
    e.cost_estimate().budget().reset_unlimited();
    e.mock_all_auths_allowing_non_root_auth();
    let wasm_hash = e.deployer().upload_contract_wasm(pool::WASM);

    let backstop_id = Address::generate(&e);
    let blnd_id = Address::generate(&e);

    let pool_init_meta = PoolInitMeta {
        backstop: backstop_id.clone(),
        pool_hash: wasm_hash.clone(),
        blnd_id: blnd_id.clone(),
    };
    let pool_factory_address = e.register(PoolFactoryContract {}, (pool_init_meta,));
    let pool_factory_client = PoolFactoryClient::new(&e, &pool_factory_address);

    let bombadil = Address::generate(&e);
    let oracle = Address::generate(&e);
    let backstop_rate: u32 = 0_1000000;
    let max_positions: u32 = 61;
    let min_collateral: i128 = 1_0000000;

    let name1 = String::from_str(&e, "pool1");
    let salt = BytesN::<32>::random(&e);

    pool_factory_client.deploy(
        &bombadil,
        &name1,
        &salt,
        &oracle,
        &backstop_rate,
        &max_positions,
        &min_collateral,
        &backstop_config(&e),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #1300)")]
fn test_pool_factory_invalid_pool_init_args_min_collateral() {
    let e = Env::default();
    e.cost_estimate().budget().reset_unlimited();
    e.mock_all_auths_allowing_non_root_auth();
    let wasm_hash = e.deployer().upload_contract_wasm(pool::WASM);

    let backstop_id = Address::generate(&e);
    let blnd_id = Address::generate(&e);

    let pool_init_meta = PoolInitMeta {
        backstop: backstop_id.clone(),
        pool_hash: wasm_hash.clone(),
        blnd_id: blnd_id.clone(),
    };
    let pool_factory_address = e.register(PoolFactoryContract {}, (pool_init_meta,));
    let pool_factory_client = PoolFactoryClient::new(&e, &pool_factory_address);

    let bombadil = Address::generate(&e);
    let oracle = Address::generate(&e);
    let backstop_rate: u32 = 0_1000000;
    let max_positions: u32 = 60;
    let min_collateral: i128 = -1;

    let name1 = String::from_str(&e, "pool1");
    let salt = BytesN::<32>::random(&e);

    pool_factory_client.deploy(
        &bombadil,
        &name1,
        &salt,
        &oracle,
        &backstop_rate,
        &max_positions,
        &min_collateral,
        &backstop_config(&e),
    );
}

#[test]
fn test_pool_factory_frontrun_protection() {
    let e = Env::default();
    e.cost_estimate().budget().reset_unlimited();
    e.mock_all_auths();

    let wasm_hash = e.deployer().upload_contract_wasm(pool::WASM);

    let bombadil = Address::generate(&e);
    let sauron = Address::generate(&e);

    let oracle = Address::generate(&e);
    let backstop_id = Address::generate(&e);
    let backstop_rate: u32 = 0_1000000;
    let max_positions: u32 = 6;
    let min_collateral: i128 = 0;
    let blnd_id = Address::generate(&e);

    let pool_init_meta = PoolInitMeta {
        backstop: backstop_id.clone(),
        pool_hash: wasm_hash.clone(),
        blnd_id: blnd_id.clone(),
    };
    let pool_factory_address = e.register(PoolFactoryContract {}, (pool_init_meta,));
    let pool_factory_client = PoolFactoryClient::new(&e, &pool_factory_address);

    let name1 = String::from_str(&e, "pool1");
    let name2 = String::from_str(&e, "pool_front_run");
    let salt = BytesN::<32>::random(&e);

    // verify two different users don't get the same pool address with the same
    // salt parameter
    let deployed_pool_address_sauron = pool_factory_client.deploy(
        &sauron,
        &name2,
        &salt,
        &oracle,
        &backstop_rate,
        &max_positions,
        &min_collateral,
        &backstop_config(&e),
    );

    let deployed_pool_address_bombadil = pool_factory_client.deploy(
        &bombadil,
        &name1,
        &salt,
        &oracle,
        &backstop_rate,
        &max_positions,
        &min_collateral,
        &backstop_config(&e),
    );

    assert!(deployed_pool_address_sauron != deployed_pool_address_bombadil);
    assert!(pool_factory_client.is_pool(&deployed_pool_address_sauron));
    assert!(pool_factory_client.is_pool(&deployed_pool_address_bombadil));
}
