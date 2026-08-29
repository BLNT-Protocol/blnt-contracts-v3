use crate::{errors::EmitterError, storage};
use sep_41_token::{StellarAssetClient, TokenClient};
use soroban_sdk::{panic_with_error, Address, Env};

pub fn execute_swap_blnd_for_blnt(e: &Env, from: &Address, to: &Address, amount: i128) -> i128 {
    if !storage::get_is_init(e) {
        panic_with_error!(e, EmitterError::NotInitialized);
    }
    if amount <= 0 {
        panic_with_error!(e, EmitterError::InvalidSwapAmount);
    }
    if e.ledger().timestamp() >= storage::get_swap_deadline(e) {
        panic_with_error!(e, EmitterError::SwapWindowClosed);
    }
    if storage::get_swap_lock(e) {
        panic_with_error!(e, EmitterError::ReentrantSwap);
    }

    from.require_auth();
    let legacy_blnd = storage::get_legacy_blnd_token(e);
    let blnt = storage::get_emission_token(e);
    if legacy_blnd == blnt {
        panic_with_error!(e, EmitterError::InvalidSwapToken);
    }

    storage::set_swap_lock(e, true);
    let emitter = e.current_contract_address();
    let legacy = TokenClient::new(e, &legacy_blnd);
    let blnt_token = TokenClient::new(e, &blnt);
    let legacy_before = legacy.balance(&emitter);
    let blnt_before = blnt_token.balance(to);

    legacy.transfer(from, &emitter, &amount);
    if legacy.balance(&emitter).checked_sub(legacy_before) != Some(amount) {
        panic_with_error!(e, EmitterError::SwapBalanceError);
    }
    legacy.burn(&emitter, &amount);
    if legacy.balance(&emitter) != legacy_before {
        panic_with_error!(e, EmitterError::SwapBalanceError);
    }

    StellarAssetClient::new(e, &blnt).mint(to, &amount);
    if blnt_token.balance(to).checked_sub(blnt_before) != Some(amount) {
        panic_with_error!(e, EmitterError::SwapBalanceError);
    }

    let total = storage::get_total_swapped(e)
        .checked_add(amount)
        .unwrap_or_else(|| panic_with_error!(e, EmitterError::OverflowError));
    storage::set_total_swapped(e, total);
    storage::set_swap_lock(e, false);
    total
}

#[cfg(test)]
mod tests {
    use crate::{testutils::create_emitter_with_legacy, EmitterClient, SWAP_WINDOW_SECONDS};
    use sep_41_token::testutils::MockTokenClient;
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        Address, Env,
    };

    struct Fixture {
        e: Env,
        emitter: Address,
        legacy: Address,
        blnt: Address,
        user: Address,
    }

    impl Fixture {
        fn create() -> Self {
            let e = Env::default();
            e.mock_all_auths();
            e.ledger().set_timestamp(1_000);
            let admin = Address::generate(&e);
            let legacy = e.register_stellar_asset_contract_v2(admin).address();
            let emitter = create_emitter_with_legacy(&e, &legacy);
            let blnt = e
                .register_stellar_asset_contract_v2(emitter.clone())
                .address();
            let backstop = Address::generate(&e);
            let backstop_token = Address::generate(&e);
            let client = EmitterClient::new(&e, &emitter);
            client.initialize(&blnt, &backstop, &backstop_token);
            let user = Address::generate(&e);
            let legacy_client = MockTokenClient::new(&e, &legacy);
            legacy_client.mint(&user, &(100 * 10_000_000));
            Self {
                e,
                emitter,
                legacy,
                blnt,
                user,
            }
        }

        fn client(&self) -> EmitterClient<'_> {
            EmitterClient::new(&self.e, &self.emitter)
        }
    }

    #[test]
    fn swaps_blnd_for_blnt_one_to_one_and_burns_receipt() {
        let fixture = Fixture::create();
        let recipient = Address::generate(&fixture.e);
        let amount = 25 * 10_000_000;
        let legacy_client = MockTokenClient::new(&fixture.e, &fixture.legacy);
        let blnt_client = MockTokenClient::new(&fixture.e, &fixture.blnt);

        assert_eq!(
            fixture
                .client()
                .swap_blnd_for_blnt(&fixture.user, &recipient, &amount),
            amount
        );
        assert_eq!(legacy_client.balance(&fixture.user), 75 * 10_000_000);
        assert_eq!(legacy_client.balance(&fixture.emitter), 0);
        assert_eq!(blnt_client.balance(&recipient), amount);
        assert_eq!(fixture.client().get_total_swapped(), amount);
        assert_eq!(fixture.client().get_legacy_blnd_token(), fixture.legacy);
        assert_ne!(fixture.legacy, fixture.blnt);
    }

    #[test]
    fn permits_swap_until_but_not_at_the_exclusive_deadline() {
        let fixture = Fixture::create();
        assert_eq!(
            fixture.client().get_swap_deadline(),
            1_000 + SWAP_WINDOW_SECONDS
        );
        fixture
            .e
            .ledger()
            .set_timestamp(1_000 + SWAP_WINDOW_SECONDS - 1);
        fixture
            .client()
            .swap_blnd_for_blnt(&fixture.user, &fixture.user, &1);
        fixture
            .e
            .ledger()
            .set_timestamp(1_000 + SWAP_WINDOW_SECONDS);
        assert!(fixture
            .client()
            .try_swap_blnd_for_blnt(&fixture.user, &fixture.user, &1)
            .is_err());
    }

    #[test]
    fn rejects_nonpositive_swap_amounts() {
        let fixture = Fixture::create();
        assert!(fixture
            .client()
            .try_swap_blnd_for_blnt(&fixture.user, &fixture.user, &0)
            .is_err());
        assert!(fixture
            .client()
            .try_swap_blnd_for_blnt(&fixture.user, &fixture.user, &-1)
            .is_err());
    }

    #[test]
    fn initialization_rejects_blnt_not_administered_by_the_emitter() {
        let e = Env::default();
        e.mock_all_auths();
        let admin = Address::generate(&e);
        let legacy = e
            .register_stellar_asset_contract_v2(admin.clone())
            .address();
        let blnt = e.register_stellar_asset_contract_v2(admin).address();
        let emitter = create_emitter_with_legacy(&e, &legacy);
        let client = EmitterClient::new(&e, &emitter);

        assert!(client
            .try_initialize(&blnt, &Address::generate(&e), &Address::generate(&e))
            .is_err());
    }
}
