#![cfg(test)]

use backstop::{BackstopContract, BackstopTier};
use mock_pool_factory::{MockPoolFactory, PoolInitMeta};
use soroban_sdk::{
    testutils::{Address as _, AuthorizedFunction, AuthorizedInvocation},
    vec, Address, BytesN, Env, IntoVal, Symbol, Val, Vec,
};
use test_suites::{
    assertions::event_from_end,
    create_fixture_with_data,
    liquidity_pool::create_lp_pool,
    test_fixture::{TokenIndex, SCALAR_7},
    token::create_stellar_token,
};

fn create_backstop_assets(e: &Env) -> (Address, Address, Address, Address, Address) {
    let admin = Address::generate(e);
    let (blnt, _) = create_stellar_token(e, &admin);
    let (usdc, _) = create_stellar_token(e, &admin);
    let (xlm, _) = create_stellar_token(e, &admin);
    let (blnt_usdc, _) = create_lp_pool(e, &admin, &blnt, &usdc);
    let (blnt_xlm, _) = create_lp_pool(e, &admin, &blnt, &xlm);
    (blnt, usdc, xlm, blnt_usdc, blnt_xlm)
}

/// Test user exposed functions on the backstop for basic functionality, auth, and events.
/// Does not test internal state management of the backstop, only external effects.
#[test]
fn test_backstop() {
    let fixture = create_fixture_with_data(false);
    let frodo = fixture.users.get(0).unwrap();

    let pool = &fixture.pools[0].pool;
    let bstop_token = &fixture.lp;
    let sam = Address::generate(&fixture.env);

    // Verify constructor bound the BLNT:USDC tier token.
    assert_eq!(
        fixture
            .backstop
            .backstop_token(&BackstopTier::SecondLoss, &pool.address),
        bstop_token.address.clone()
    );

    // Mint some backstop tokens
    // assumes Sam makes up 20% of the backstop after depositing (50k / 0.8 * 0.2 = 12.5k)
    //  -> mint 12.5k LP tokens to sam
    fixture.tokens[TokenIndex::BLNT].mint(&sam, &(125_001_000_0000_0000_000_000 * SCALAR_7)); // 10 BLNT per LP token
    fixture.tokens[TokenIndex::BLNT].approve(&sam, &bstop_token.address, &i128::MAX, &99999);
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
    fixture.tokens[TokenIndex::BLNT].mint(&frodo, &(50_001 * SCALAR_7)); // 10 BLNT per LP token
    fixture.tokens[TokenIndex::BLNT].approve(&frodo, &bstop_token.address, &i128::MAX, &99999);
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
    let result = fixture.backstop.deposit(
        &backstop::BackstopTier::SecondLoss,
        &sam,
        &pool.address,
        &amount,
    );
    sam_bstop_token_balance -= amount;
    bstop_bstop_token_balance += amount;
    assert_eq!(
        fixture.env.auths()[0],
        (
            sam.clone(),
            AuthorizedInvocation {
                function: AuthorizedFunction::Contract((
                    fixture.backstop.address.clone(),
                    Symbol::new(&fixture.env, "deposit"),
                    vec![
                        &fixture.env,
                        BackstopTier::SecondLoss.into_val(&fixture.env),
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
                    BackstopTier::SecondLoss,
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
    fixture
        .backstop
        .donate(&BackstopTier::SecondLoss, &frodo, &pool.address, &amount);
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
                        BackstopTier::SecondLoss.into_val(&fixture.env),
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
                        BackstopTier::SecondLoss.into_val(&fixture.env),
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
                    BackstopTier::SecondLoss,
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
    let result = fixture.backstop.queue_withdrawal(
        &backstop::BackstopTier::SecondLoss,
        &sam,
        &pool.address,
        &amount,
    );
    assert_eq!(
        fixture.env.auths()[0],
        (
            sam.clone(),
            AuthorizedInvocation {
                function: AuthorizedFunction::Contract((
                    fixture.backstop.address.clone(),
                    Symbol::new(&fixture.env, "queue_withdrawal"),
                    vec![
                        &fixture.env,
                        BackstopTier::SecondLoss.into_val(&fixture.env),
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
                    BackstopTier::SecondLoss,
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
    fixture.backstop.dequeue_withdrawal(
        &backstop::BackstopTier::SecondLoss,
        &sam,
        &pool.address,
        &amount,
    );
    assert_eq!(
        fixture.env.auths()[0],
        (
            sam.clone(),
            AuthorizedInvocation {
                function: AuthorizedFunction::Contract((
                    fixture.backstop.address.clone(),
                    Symbol::new(&fixture.env, "dequeue_withdrawal"),
                    vec![
                        &fixture.env,
                        BackstopTier::SecondLoss.into_val(&fixture.env),
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
                    BackstopTier::SecondLoss,
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
    fixture
        .backstop
        .draw(&BackstopTier::SecondLoss, &pool.address, &amount, &frodo);
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
                        BackstopTier::SecondLoss.into_val(&fixture.env),
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
                (
                    Symbol::new(&fixture.env, "draw"),
                    BackstopTier::SecondLoss,
                    pool.address.clone()
                )
                    .into_val(&fixture.env),
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
    let result = fixture.backstop.withdraw(
        &backstop::BackstopTier::SecondLoss,
        &sam,
        &pool.address,
        &amount,
        &sam,
    );
    sam_bstop_token_balance += result; // sam caught 20% of 1k profit and is withdrawing half his position
    bstop_bstop_token_balance -= result;
    assert_eq!(
        fixture.env.auths()[0],
        (
            sam.clone(),
            AuthorizedInvocation {
                function: AuthorizedFunction::Contract((
                    fixture.backstop.address.clone(),
                    Symbol::new(&fixture.env, "withdraw"),
                    vec![
                        &fixture.env,
                        BackstopTier::SecondLoss.into_val(&fixture.env),
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
                    BackstopTier::SecondLoss,
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

    // Sam compounds BLNT emissions into the originating BLNT:USDC tier.
    let bstop_blend_balance = fixture.tokens[TokenIndex::BLNT].balance(&fixture.backstop.address);
    let bstop_lp_balance = bstop_token.balance(&fixture.backstop.address);
    let sam_blend_balance = fixture.tokens[TokenIndex::BLNT].balance(&sam);
    let sam_shares = fixture
        .backstop
        .user_balance(&BackstopTier::SecondLoss, &pool.address, &sam)
        .shares;
    let lp_compounded = fixture.backstop.claim(
        &BackstopTier::SecondLoss,
        &sam,
        &vec![&fixture.env, pool.address.clone()],
        &0,
    );
    assert_eq!(
        fixture.env.auths()[0],
        (
            sam.clone(),
            AuthorizedInvocation {
                function: AuthorizedFunction::Contract((
                    fixture.backstop.address.clone(),
                    Symbol::new(&fixture.env, "claim"),
                    vec![
                        &fixture.env,
                        BackstopTier::SecondLoss.into_val(&fixture.env),
                        sam.to_val(),
                        vec![&fixture.env, pool.address.clone()].to_val(),
                        0_i128.into_val(&fixture.env),
                    ]
                )),
                sub_invocations: std::vec![]
            }
        )
    );
    let event = vec![&fixture.env, event_from_end(&fixture.env, 1)];

    let blnt_compounded =
        bstop_blend_balance - fixture.tokens[TokenIndex::BLNT].balance(&fixture.backstop.address);
    let shares_minted = fixture
        .backstop
        .user_balance(&BackstopTier::SecondLoss, &pool.address, &sam)
        .shares
        - sam_shares;
    assert_eq!(
        event,
        vec![
            &fixture.env,
            (
                fixture.backstop.address.clone(),
                (
                    Symbol::new(&fixture.env, "claim"),
                    BackstopTier::SecondLoss,
                    sam.clone(),
                    pool.address.clone()
                )
                    .into_val(&fixture.env),
                (blnt_compounded, lp_compounded, shares_minted).into_val(&fixture.env),
            )
        ]
    );

    assert!(blnt_compounded > 0);
    assert_eq!(
        bstop_token.balance(&fixture.backstop.address),
        bstop_lp_balance + lp_compounded
    );
    assert!(lp_compounded > 0);
    assert!(shares_minted > 0);
    assert_eq!(
        fixture.tokens[TokenIndex::BLNT].balance(&sam),
        sam_blend_balance
    );
}

#[test]
fn test_backstop_constructor() {
    let e = Env::default();
    e.mock_all_auths();

    let (blnt_token, usdc_token, xlm_token, backstop_token, blnt_xlm_token) =
        create_backstop_assets(&e);
    let emitter = Address::generate(&e);
    let contract_id = Address::generate(&e);
    let recipient = Address::generate(&e);
    let drop_list = vec![&e, (recipient.clone(), 40_000_000 * SCALAR_7)];
    let pool_factory = e.register(
        MockPoolFactory {},
        (PoolInitMeta {
            backstop: contract_id.clone(),
            pool_hash: BytesN::from_array(&e, &[0; 32]),
            blnt_id: blnt_token.clone(),
        },),
    );
    e.register_at(
        &contract_id,
        BackstopContract {},
        (
            backstop_token.clone(),
            blnt_xlm_token.clone(),
            emitter.clone(),
            blnt_token.clone(),
            usdc_token.clone(),
            xlm_token.clone(),
            pool_factory.clone(),
            drop_list.clone(),
        ),
    );

    e.as_contract(&contract_id, || {
        let contract_emitter = e
            .storage()
            .instance()
            .get::<Symbol, Address>(&Symbol::new(&e, "Emitter"))
            .unwrap();
        assert_eq!(contract_emitter, emitter);

        let contract_blnt_token = e
            .storage()
            .instance()
            .get::<Symbol, Address>(&Symbol::new(&e, "BLNTTkn"))
            .unwrap();
        assert_eq!(contract_blnt_token, blnt_token);

        let contract_usdc_token = e
            .storage()
            .instance()
            .get::<Symbol, Address>(&Symbol::new(&e, "USDCTkn"))
            .unwrap();
        assert_eq!(contract_usdc_token, usdc_token);

        let contract_xlm_token = e
            .storage()
            .instance()
            .get::<Symbol, Address>(&Symbol::new(&e, "XLMTkn"))
            .unwrap();
        assert_eq!(contract_xlm_token, xlm_token);

        let contract_blnt_xlm_token = e
            .storage()
            .instance()
            .get::<Symbol, Address>(&Symbol::new(&e, "BXLMTkn"))
            .unwrap();
        assert_eq!(contract_blnt_xlm_token, blnt_xlm_token);

        let contract_pool_factory = e
            .storage()
            .instance()
            .get::<Symbol, Address>(&Symbol::new(&e, "PoolFact"))
            .unwrap();
        assert_eq!(contract_pool_factory, pool_factory);

        let contract_drop_list = e
            .storage()
            .persistent()
            .get::<Symbol, Vec<(Address, i128)>>(&Symbol::new(&e, "DropList"))
            .unwrap();
        assert_eq!(contract_drop_list, drop_list);
    });
}

#[test]
#[should_panic(expected = "Error(Contract, #1014)")]
fn test_backstop_constructor_rejects_wrong_comet_pair() {
    let e = Env::default();
    e.mock_all_auths();
    let contract_id = Address::generate(&e);
    let (blnt_token, usdc_token, xlm_token, blnt_usdc_token, _) = create_backstop_assets(&e);
    let admin = Address::generate(&e);
    let (wrong_blnt_xlm, _) = create_lp_pool(&e, &admin, &blnt_token, &usdc_token);
    let pool_factory = e.register(
        MockPoolFactory {},
        (PoolInitMeta {
            backstop: contract_id.clone(),
            pool_hash: BytesN::from_array(&e, &[0; 32]),
            blnt_id: blnt_token.clone(),
        },),
    );
    e.register_at(
        &contract_id,
        BackstopContract {},
        (
            blnt_usdc_token,
            wrong_blnt_xlm,
            Address::generate(&e),
            blnt_token,
            usdc_token,
            xlm_token,
            pool_factory,
            Vec::<(Address, i128)>::new(&e),
        ),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #1000)")]
fn test_backstop_constructor_rejects_drop_list_over_cap() {
    let e = Env::default();
    e.mock_all_auths();
    let contract_id = Address::generate(&e);
    let (blnt, usdc, xlm, blnt_usdc, blnt_xlm) = create_backstop_assets(&e);
    let pool_factory = e.register(
        MockPoolFactory {},
        (PoolInitMeta {
            backstop: contract_id.clone(),
            pool_hash: BytesN::from_array(&e, &[0; 32]),
            blnt_id: blnt.clone(),
        },),
    );
    let drop_list = vec![&e, (Address::generate(&e), 50_000_000 * SCALAR_7 + 1)];
    e.register_at(
        &contract_id,
        BackstopContract {},
        (
            blnt_usdc,
            blnt_xlm,
            Address::generate(&e),
            blnt,
            usdc,
            xlm,
            pool_factory,
            drop_list,
        ),
    );
}
