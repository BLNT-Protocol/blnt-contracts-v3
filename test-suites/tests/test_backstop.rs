#![cfg(test)]

use backstop::{BackstopClient, BackstopContract, BackstopTier};
use mock_pool_factory::{MockPoolFactory, PoolInitMeta};
use soroban_fixed_point_math::FixedPoint;
use soroban_sdk::{
    testutils::{Address as _, AuthorizedFunction, AuthorizedInvocation},
    vec, Address, BytesN, Env, IntoVal, Symbol, Val, Vec,
};
use test_suites::{
    assertions::{assert_approx_eq_abs, event_from_end},
    backstop::{create_mock_backstop_valuation, set_mock_backstop_valuation_version},
    create_fixture_with_data,
    test_fixture::{TokenIndex, SCALAR_7},
};

/// Test user exposed functions on the backstop for basic functionality, auth, and events.
/// Does not test internal state management of the backstop, only external effects.
#[test]
fn test_backstop() {
    let fixture = create_fixture_with_data(false);
    let frodo = fixture.users.get(0).unwrap();

    let pool = &fixture.pools[0].pool;
    let bstop_token = &fixture.lp;
    let sam = Address::generate(&fixture.env);

    // Verify constructor set the backstop token
    assert_eq!(
        fixture.backstop.backstop_token(),
        bstop_token.address.clone()
    );

    // Mint some backstop tokens
    // assumes Sam makes up 20% of the backstop after depositing (50k / 0.8 * 0.2 = 12.5k)
    //  -> mint 12.5k LP tokens to sam
    fixture.tokens[TokenIndex::BLND].mint(&sam, &(125_001_000_0000_0000_000_000 * SCALAR_7)); // 10 BLND per LP token
    fixture.tokens[TokenIndex::BLND].approve(&sam, &bstop_token.address, &i128::MAX, &99999);
    fixture.tokens[TokenIndex::USDC].mint(&sam, &(3_126_000_0000_0000_000_000 * SCALAR_7)); // 0.25 USDC per LP token
    fixture.tokens[TokenIndex::USDC].approve(&sam, &bstop_token.address, &i128::MAX, &99999);
    bstop_token.join_pool(
        &(12_500 * SCALAR_7),
        &vec![
            &fixture.env,
            125_001_000_0000_0000_000 * SCALAR_7,
            3_126_000_0000_0000_000 * SCALAR_7,
        ],
        &sam,
    );

    //  -> mint Frodo additional backstop tokens (5k) for donation later
    fixture.tokens[TokenIndex::BLND].mint(&frodo, &(50_001 * SCALAR_7)); // 10 BLND per LP token
    fixture.tokens[TokenIndex::BLND].approve(&frodo, &bstop_token.address, &i128::MAX, &99999);
    fixture.tokens[TokenIndex::USDC].mint(&frodo, &(1_251 * SCALAR_7)); // 0.25 USDC per LP token
    fixture.tokens[TokenIndex::USDC].approve(&frodo, &bstop_token.address, &i128::MAX, &99999);
    bstop_token.join_pool(
        &(5_000 * SCALAR_7),
        &vec![&fixture.env, 50_001 * SCALAR_7, 1_251 * SCALAR_7],
        &frodo,
    );

    let mut frodo_bstop_token_balance = bstop_token.balance(&frodo);
    let mut bstop_bstop_token_balance = bstop_token.balance(&fixture.backstop.address);
    let mut sam_bstop_token_balance = bstop_token.balance(&sam);
    assert_eq!(sam_bstop_token_balance, 12_500 * SCALAR_7);

    // Sam deposits 12.5k backstop tokens
    // Refresh the ongoing-emission checkpoint immediately before changing an
    // active reward-zone weight.
    fixture.backstop.distribute();
    let amount = 12_500 * SCALAR_7;
    let result = fixture
        .backstop
        .deposit_blnd_usdc(&sam, &pool.address, &amount);
    sam_bstop_token_balance -= amount;
    bstop_bstop_token_balance += amount;
    assert_eq!(
        fixture.env.auths()[0],
        (
            sam.clone(),
            AuthorizedInvocation {
                function: AuthorizedFunction::Contract((
                    fixture.backstop.address.clone(),
                    Symbol::new(&fixture.env, "deposit_blnd_usdc"),
                    vec![
                        &fixture.env,
                        sam.to_val(),
                        pool.address.to_val(),
                        amount.into_val(&fixture.env)
                    ]
                )),
                sub_invocations: std::vec![AuthorizedInvocation {
                    function: AuthorizedFunction::Contract((
                        bstop_token.address.clone(),
                        Symbol::new(&fixture.env, "transfer"),
                        vec![
                            &fixture.env,
                            sam.to_val(),
                            fixture.backstop.address.to_val(),
                            amount.into_val(&fixture.env)
                        ]
                    )),
                    sub_invocations: std::vec![]
                }]
            }
        )
    );
    let event = vec![&fixture.env, event_from_end(&fixture.env, 1)];
    let event_body: Vec<Val> = vec![
        &fixture.env,
        amount.into_val(&fixture.env),
        result.into_val(&fixture.env),
    ];
    assert_eq!(
        event,
        vec![
            &fixture.env,
            (
                fixture.backstop.address.clone(),
                (
                    Symbol::new(&fixture.env, "deposit"),
                    BackstopTier::BlndUsdc,
                    pool.address.clone(),
                    sam.clone()
                )
                    .into_val(&fixture.env),
                event_body.into_val(&fixture.env)
            )
        ]
    );
    assert_eq!(result, amount);
    assert_eq!(bstop_token.balance(&sam), sam_bstop_token_balance);
    assert_eq!(
        bstop_token.balance(&fixture.backstop.address),
        bstop_bstop_token_balance
    );

    // Simulate the pool backstop making money and progress 6d23h (+6d23hr 20% emissions for sam)
    fixture.jump(60 * 60 * 24 * 7 - 60 * 60);
    // Start the next emission cycle
    fixture.backstop.distribute();
    let event = vec![&fixture.env, event_from_end(&fixture.env, 1)];
    assert_eq!(
        event,
        vec![
            &fixture.env,
            (
                fixture.backstop.address.clone(),
                (Symbol::new(&fixture.env, "distribute"),).into_val(&fixture.env),
                ((60 * 60 * 24 * 7 - 60 * 60) * SCALAR_7).into_val(&fixture.env),
            )
        ]
    );
    pool.gulp_emissions();
    let amount = 2_000 * SCALAR_7;
    fixture.lp.approve(
        &frodo,
        &fixture.backstop.address,
        &amount,
        &fixture.env.ledger().sequence(),
    );
    fixture.backstop.donate(&frodo, &pool.address, &amount);
    frodo_bstop_token_balance -= amount;
    bstop_bstop_token_balance += amount;
    assert_eq!(
        fixture.env.auths()[0],
        (
            frodo.clone(),
            AuthorizedInvocation {
                function: AuthorizedFunction::Contract((
                    fixture.backstop.address.clone(),
                    Symbol::new(&fixture.env, "donate"),
                    vec![
                        &fixture.env,
                        frodo.to_val(),
                        pool.address.to_val(),
                        amount.into_val(&fixture.env)
                    ]
                )),
                sub_invocations: std::vec![]
            }
        )
    );
    assert_eq!(
        fixture.env.auths()[1],
        (
            pool.address.clone(),
            AuthorizedInvocation {
                function: AuthorizedFunction::Contract((
                    fixture.backstop.address.clone(),
                    Symbol::new(&fixture.env, "donate"),
                    vec![
                        &fixture.env,
                        frodo.to_val(),
                        pool.address.to_val(),
                        amount.into_val(&fixture.env)
                    ]
                )),
                sub_invocations: std::vec![]
            }
        )
    );
    let event = vec![&fixture.env, event_from_end(&fixture.env, 1)];
    assert_eq!(
        event,
        vec![
            &fixture.env,
            (
                fixture.backstop.address.clone(),
                (
                    Symbol::new(&fixture.env, "donate"),
                    pool.address.clone(),
                    frodo.clone()
                )
                    .into_val(&fixture.env),
                amount.into_val(&fixture.env)
            )
        ]
    );
    assert_eq!(bstop_token.balance(&frodo), frodo_bstop_token_balance);
    assert_eq!(
        bstop_token.balance(&fixture.backstop.address),
        bstop_bstop_token_balance
    );

    assert_eq!(fixture.env.auths().len(), 0);

    // Sam queues 100% of position for withdrawal
    let amount = 12_500 * SCALAR_7; // shares
    let result = fixture
        .backstop
        .queue_blnd_usdc_withdrawal(&sam, &pool.address, &amount);
    assert_eq!(
        fixture.env.auths()[0],
        (
            sam.clone(),
            AuthorizedInvocation {
                function: AuthorizedFunction::Contract((
                    fixture.backstop.address.clone(),
                    Symbol::new(&fixture.env, "queue_blnd_usdc_withdrawal"),
                    vec![
                        &fixture.env,
                        sam.to_val(),
                        pool.address.to_val(),
                        amount.into_val(&fixture.env)
                    ]
                )),
                sub_invocations: std::vec![]
            }
        )
    );
    let event = vec![&fixture.env, event_from_end(&fixture.env, 1)];
    let event_body: Vec<Val> = vec![
        &fixture.env,
        amount.into_val(&fixture.env),
        result.exp.into_val(&fixture.env),
    ];
    assert_eq!(
        event,
        vec![
            &fixture.env,
            (
                fixture.backstop.address.clone(),
                (
                    Symbol::new(&fixture.env, "queue_withdrawal"),
                    BackstopTier::BlndUsdc,
                    pool.address.clone(),
                    sam.clone()
                )
                    .into_val(&fixture.env),
                event_body.into_val(&fixture.env)
            )
        ]
    );
    assert_eq!(result.amount, amount);
    assert_eq!(
        result.exp,
        fixture.env.ledger().timestamp() + 17 * 24 * 60 * 60
    );
    assert_eq!(bstop_token.balance(&sam), sam_bstop_token_balance);
    assert_eq!(
        bstop_token.balance(&fixture.backstop.address),
        bstop_bstop_token_balance
    );

    // Start the next emission cycle and jump 7 days (No emissions earned for sam)
    fixture.jump(60 * 60 * 24 * 7);
    fixture.backstop.distribute();
    pool.gulp_emissions();

    // Sam dequeues half of the withdrawal
    // -> sam now makes up 11% of the unqueued shares in the backstop
    let amount = 6_250 * SCALAR_7; // shares
    fixture
        .backstop
        .dequeue_blnd_usdc_withdrawal(&sam, &pool.address, &amount);
    assert_eq!(
        fixture.env.auths()[0],
        (
            sam.clone(),
            AuthorizedInvocation {
                function: AuthorizedFunction::Contract((
                    fixture.backstop.address.clone(),
                    Symbol::new(&fixture.env, "dequeue_blnd_usdc_withdrawal"),
                    vec![
                        &fixture.env,
                        sam.to_val(),
                        pool.address.to_val(),
                        amount.into_val(&fixture.env)
                    ]
                )),
                sub_invocations: std::vec![]
            }
        )
    );
    let event = vec![&fixture.env, event_from_end(&fixture.env, 1)];
    assert_eq!(
        event,
        vec![
            &fixture.env,
            (
                fixture.backstop.address.clone(),
                (
                    Symbol::new(&fixture.env, "dequeue_withdrawal"),
                    BackstopTier::BlndUsdc,
                    pool.address.clone(),
                    sam.clone()
                )
                    .into_val(&fixture.env),
                amount.into_val(&fixture.env)
            )
        ]
    );
    assert_eq!(bstop_token.balance(&sam), sam_bstop_token_balance);
    assert_eq!(
        bstop_token.balance(&fixture.backstop.address),
        bstop_bstop_token_balance
    );

    // Start the next emission cycle and jump 7 days (+7d 11% emissions for sam)
    fixture.jump(60 * 60 * 24 * 7);
    fixture.backstop.distribute();
    pool.gulp_emissions();

    // Backstop loses money
    let amount = 1_000 * SCALAR_7;
    fixture.backstop.draw(&pool.address, &amount, &frodo);
    frodo_bstop_token_balance += amount;
    bstop_bstop_token_balance -= amount;
    assert_eq!(
        fixture.env.auths()[0],
        (
            pool.address.clone(),
            AuthorizedInvocation {
                function: AuthorizedFunction::Contract((
                    fixture.backstop.address.clone(),
                    Symbol::new(&fixture.env, "draw"),
                    vec![
                        &fixture.env,
                        pool.address.to_val(),
                        amount.into_val(&fixture.env),
                        frodo.to_val()
                    ]
                )),
                sub_invocations: std::vec![]
            }
        )
    );
    let event = vec![&fixture.env, event_from_end(&fixture.env, 1)];
    assert_eq!(
        event,
        vec![
            &fixture.env,
            (
                fixture.backstop.address.clone(),
                (Symbol::new(&fixture.env, "draw"), pool.address.clone()).into_val(&fixture.env),
                vec![&fixture.env, frodo.to_val(), amount.into_val(&fixture.env),]
                    .into_val(&fixture.env)
            )
        ]
    );
    assert_eq!(bstop_token.balance(&frodo), frodo_bstop_token_balance);
    assert_eq!(
        bstop_token.balance(&fixture.backstop.address),
        bstop_bstop_token_balance
    );

    // Jump to the end of the withdrawal period (+16d1s at 11% for Sam).
    fixture.jump(60 * 60 * 24 * 16 + 1);
    fixture.backstop.distribute();
    // Sam withdraws the queue position
    let amount = 6_250 * SCALAR_7; // shares
    let result = fixture
        .backstop
        .withdraw_blnd_usdc(&sam, &pool.address, &amount, &sam);
    sam_bstop_token_balance += result; // sam caught 20% of 1k profit and is withdrawing half his position
    bstop_bstop_token_balance -= result;
    assert_eq!(
        fixture.env.auths()[0],
        (
            sam.clone(),
            AuthorizedInvocation {
                function: AuthorizedFunction::Contract((
                    fixture.backstop.address.clone(),
                    Symbol::new(&fixture.env, "withdraw_blnd_usdc"),
                    vec![
                        &fixture.env,
                        sam.to_val(),
                        pool.address.to_val(),
                        amount.into_val(&fixture.env),
                        sam.to_val(),
                    ]
                )),
                sub_invocations: std::vec![]
            }
        )
    );
    let event = vec![&fixture.env, event_from_end(&fixture.env, 1)];
    let event_body: Vec<Val> = vec![
        &fixture.env,
        amount.into_val(&fixture.env),
        result.into_val(&fixture.env),
    ];
    assert_eq!(
        event,
        vec![
            &fixture.env,
            (
                fixture.backstop.address.clone(),
                (
                    Symbol::new(&fixture.env, "withdraw"),
                    BackstopTier::BlndUsdc,
                    pool.address.clone(),
                    sam.clone()
                )
                    .into_val(&fixture.env),
                event_body.into_val(&fixture.env)
            )
        ]
    );
    assert_eq!(result, amount + 100 * SCALAR_7); // sam due 20% of 1k profit. Captures half (100) since withdrawing half his position.
    assert_eq!(bstop_token.balance(&sam), sam_bstop_token_balance);
    assert_eq!(
        bstop_token.balance(&fixture.backstop.address),
        bstop_bstop_token_balance
    );

    // Sam claims direct BLND emissions earned on the backstop deposit.
    let bstop_blend_balance = &fixture.tokens[TokenIndex::BLND].balance(&fixture.backstop.address);
    let sam_blend_balance = fixture.tokens[TokenIndex::BLND].balance(&sam);
    let blnd_claimed = fixture
        .backstop
        .claim_ongoing_blnd(&sam, &pool.address, &sam);
    assert_eq!(
        fixture.env.auths()[0],
        (
            sam.clone(),
            AuthorizedInvocation {
                function: AuthorizedFunction::Contract((
                    fixture.backstop.address.clone(),
                    Symbol::new(&fixture.env, "claim_ongoing_blnd"),
                    vec![
                        &fixture.env,
                        sam.to_val(),
                        pool.address.to_val(),
                        sam.to_val(),
                    ]
                )),
                sub_invocations: std::vec![]
            }
        )
    );

    // 6d23hr at 20% of 0.7 BLND/sec
    // 7d + 16d1s at 11% of 0.7 BLND/sec
    let emission_share_1 = 0_7000000.fixed_mul_floor(0_2000000, SCALAR_7).unwrap();
    let emission_share_2 = 0_7000000.fixed_mul_floor(0_1111111, SCALAR_7).unwrap();
    let emitted_blnd_1 = ((7 * 24 * 60 * 60 - 60 * 60) * SCALAR_7)
        .fixed_mul_floor(emission_share_1, SCALAR_7)
        .unwrap();
    let emitted_blnd_2 = ((23 * 24 * 60 * 60 + 1) * SCALAR_7)
        .fixed_mul_floor(emission_share_2, SCALAR_7)
        .unwrap();
    let event = vec![&fixture.env, event_from_end(&fixture.env, 1)];
    assert_eq!(
        event,
        vec![
            &fixture.env,
            (
                fixture.backstop.address.clone(),
                (
                    Symbol::new(&fixture.env, "claim_ongoing"),
                    sam.clone(),
                    pool.address.clone()
                )
                    .into_val(&fixture.env),
                (sam.clone(), blnd_claimed).into_val(&fixture.env),
            )
        ]
    );

    assert_approx_eq_abs(blnd_claimed, emitted_blnd_1 + emitted_blnd_2, SCALAR_7);
    assert_eq!(
        fixture.tokens[TokenIndex::BLND].balance(&sam),
        sam_blend_balance + blnd_claimed
    );
    assert_approx_eq_abs(
        bstop_blend_balance - fixture.tokens[TokenIndex::BLND].balance(&fixture.backstop.address),
        blnd_claimed,
        SCALAR_7,
    );
}

#[test]
fn test_backstop_constructor() {
    let e = Env::default();

    let backstop_token = Address::generate(&e);
    let blnd_xlm_token = Address::generate(&e);
    let emitter = Address::generate(&e);
    let blnd_token = Address::generate(&e);
    let usdc_token = Address::generate(&e);
    let contract_id = Address::generate(&e);
    let pool_factory = e.register(
        MockPoolFactory {},
        (PoolInitMeta {
            backstop: contract_id.clone(),
            pool_hash: BytesN::from_array(&e, &[0; 32]),
            blnd_id: blnd_token.clone(),
        },),
    );
    let drop_list: Vec<(Address, i128)> = vec![
        &e,
        (Address::generate(&e), 10_000_000_0000000),
        (Address::generate(&e), 10_000_000_0000000),
        (Address::generate(&e), 10_000_000_0000000),
        (Address::generate(&e), 10_000_000_0000000),
    ];
    let backstop_valuation = create_mock_backstop_valuation(
        &e,
        &blnd_token,
        &backstop_token,
        &blnd_xlm_token,
        &usdc_token,
    );
    e.register_at(
        &contract_id,
        BackstopContract {},
        (
            backstop_token.clone(),
            blnd_xlm_token.clone(),
            emitter.clone(),
            blnd_token.clone(),
            usdc_token.clone(),
            pool_factory.clone(),
            backstop_valuation.clone(),
            drop_list.clone(),
        ),
    );

    e.as_contract(&contract_id, || {
        let contract_drop_list = e
            .storage()
            .persistent()
            .get::<Symbol, Vec<(Address, i128)>>(&Symbol::new(&e, "DropList"))
            .unwrap();
        assert_eq!(contract_drop_list, drop_list);

        let contract_emitter = e
            .storage()
            .instance()
            .get::<Symbol, Address>(&Symbol::new(&e, "Emitter"))
            .unwrap();
        assert_eq!(contract_emitter, emitter);

        let contract_blnd_token = e
            .storage()
            .instance()
            .get::<Symbol, Address>(&Symbol::new(&e, "BLNDTkn"))
            .unwrap();
        assert_eq!(contract_blnd_token, blnd_token);

        let contract_usdc_token = e
            .storage()
            .instance()
            .get::<Symbol, Address>(&Symbol::new(&e, "USDCTkn"))
            .unwrap();
        assert_eq!(contract_usdc_token, usdc_token);

        let contract_blnd_xlm_token = e
            .storage()
            .instance()
            .get::<Symbol, Address>(&Symbol::new(&e, "BXLMTkn"))
            .unwrap();
        assert_eq!(contract_blnd_xlm_token, blnd_xlm_token);

        let contract_pool_factory = e
            .storage()
            .instance()
            .get::<Symbol, Address>(&Symbol::new(&e, "PoolFact"))
            .unwrap();
        assert_eq!(contract_pool_factory, pool_factory);

        let contract_backstop_valuation = e
            .storage()
            .instance()
            .get::<Symbol, Address>(&Symbol::new(&e, "BstopVal"))
            .unwrap();
        assert_eq!(contract_backstop_valuation, backstop_valuation);
    });

    let backstop_client = BackstopClient::new(&e, &contract_id);
    assert_eq!(backstop_client.backstop_token(), backstop_token);
}

#[test]
#[should_panic(expected = "Error(Contract, #1012)")]
fn test_backstop_constructor_rejects_wrong_factory_binding() {
    let e = Env::default();
    let contract_id = Address::generate(&e);
    let pool_factory = e.register(
        MockPoolFactory {},
        (PoolInitMeta {
            backstop: Address::generate(&e),
            pool_hash: BytesN::from_array(&e, &[0; 32]),
            blnd_id: Address::generate(&e),
        },),
    );
    e.register_at(
        &contract_id,
        BackstopContract {},
        (
            Address::generate(&e),
            Address::generate(&e),
            Address::generate(&e),
            Address::generate(&e),
            Address::generate(&e),
            pool_factory,
            Address::generate(&e),
            Vec::<(Address, i128)>::new(&e),
        ),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #1014)")]
fn test_backstop_constructor_rejects_wrong_valuation_binding() {
    let e = Env::default();
    let contract_id = Address::generate(&e);
    let blnd_usdc_token = Address::generate(&e);
    let blnd_xlm_token = Address::generate(&e);
    let blnd_token = Address::generate(&e);
    let usdc_token = Address::generate(&e);
    let pool_factory = e.register(
        MockPoolFactory {},
        (PoolInitMeta {
            backstop: contract_id.clone(),
            pool_hash: BytesN::from_array(&e, &[0; 32]),
            blnd_id: blnd_token.clone(),
        },),
    );
    let wrong_valuation = create_mock_backstop_valuation(
        &e,
        &blnd_token,
        &Address::generate(&e),
        &blnd_xlm_token,
        &usdc_token,
    );

    e.register_at(
        &contract_id,
        BackstopContract {},
        (
            blnd_usdc_token,
            blnd_xlm_token,
            Address::generate(&e),
            blnd_token,
            usdc_token,
            pool_factory,
            wrong_valuation,
            Vec::<(Address, i128)>::new(&e),
        ),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #1014)")]
fn test_backstop_constructor_rejects_wrong_valuation_version() {
    let e = Env::default();
    let contract_id = Address::generate(&e);
    let blnd_usdc_token = Address::generate(&e);
    let blnd_xlm_token = Address::generate(&e);
    let blnd_token = Address::generate(&e);
    let usdc_token = Address::generate(&e);
    let pool_factory = e.register(
        MockPoolFactory {},
        (PoolInitMeta {
            backstop: contract_id.clone(),
            pool_hash: BytesN::from_array(&e, &[0; 32]),
            blnd_id: blnd_token.clone(),
        },),
    );
    let valuation = create_mock_backstop_valuation(
        &e,
        &blnd_token,
        &blnd_usdc_token,
        &blnd_xlm_token,
        &usdc_token,
    );
    set_mock_backstop_valuation_version(&e, &valuation, 2);

    e.register_at(
        &contract_id,
        BackstopContract {},
        (
            blnd_usdc_token,
            blnd_xlm_token,
            Address::generate(&e),
            blnd_token,
            usdc_token,
            pool_factory,
            valuation,
            Vec::<(Address, i128)>::new(&e),
        ),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #1000)")]
fn test_backstop_constructor_over_max() {
    let e = Env::default();

    let backstop_token = Address::generate(&e);
    let blnd_xlm_token = Address::generate(&e);
    let emitter = Address::generate(&e);
    let blnd_token = Address::generate(&e);
    let usdc_token = Address::generate(&e);
    let contract_id = Address::generate(&e);
    let pool_factory = e.register(
        MockPoolFactory {},
        (PoolInitMeta {
            backstop: contract_id.clone(),
            pool_hash: BytesN::from_array(&e, &[0; 32]),
            blnd_id: blnd_token.clone(),
        },),
    );
    let drop_list: Vec<(Address, i128)> = vec![
        &e,
        (Address::generate(&e), 10_000_000_0000000),
        (Address::generate(&e), 10_000_000_0000000),
        (Address::generate(&e), 10_000_000_0000000),
        (Address::generate(&e), 10_000_000_0000001),
    ];
    let backstop_valuation = create_mock_backstop_valuation(
        &e,
        &blnd_token,
        &backstop_token,
        &blnd_xlm_token,
        &usdc_token,
    );
    e.register_at(
        &contract_id,
        BackstopContract {},
        (
            backstop_token.clone(),
            blnd_xlm_token,
            emitter.clone(),
            blnd_token.clone(),
            usdc_token.clone(),
            pool_factory.clone(),
            backstop_valuation,
            drop_list.clone(),
        ),
    );
}
