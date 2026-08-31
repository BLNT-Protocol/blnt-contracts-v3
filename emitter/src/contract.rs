// Preserve the legacy event topic and data encoding rather than migrating it to
// the SDK 27 typed-event API.
#![allow(deprecated)]

use crate::{backstop_manager, emitter, errors::EmitterError, storage};
use sep_41_token::{StellarAssetClient, TokenClient};
use soroban_sdk::{
    contract, contractclient, contractimpl, panic_with_error, Address, Env, Executable, Symbol, Vec,
};

/// ### Emitter
///
/// Emits BLNT to the v3 backstop.
#[contract]
pub struct EmitterContract;

#[contractclient(name = "EmitterClient")]
pub trait Emitter {
    /// Initialize the Emitter
    ///
    /// ### Arguments
    /// * `blnd_token` - Legacy ABI name for the BLNT token the emitter distributes
    /// * `backstop` - The backstop module address to emit to
    /// * `backstop_token` - The token the backstop takes deposits in
    fn initialize(e: Env, blnd_token: Address, backstop: Address, backstop_token: Address);

    /// Distributes BLNT tokens to the listed backstop module
    ///
    /// Only the current backstop may call this entry point. Its own
    /// `distribute` entry point remains permissionless.
    ///
    /// Returns the amount of BLNT tokens distributed.
    fn distribute(e: Env) -> i128;

    /// Fetch the last time the Emitter distributed to the backstop module
    ///
    /// ### Arguments
    /// * `backstop` - The backstop module Address ID
    fn get_last_distro(e: Env, backstop_id: Address) -> u64;

    /// Fetch the current backstop
    fn get_backstop(e: Env) -> Address;

    /// Queues up a swap of the listed backstop module and token to new addresses.
    ///
    /// ### Arguments
    /// * `new_backstop` - The Address of the new backstop module
    /// * `new_backstop_token` - The address of the new backstop token
    ///
    /// ### Errors
    /// If the input contract does not have more backstop deposits than the listed backstop module of the
    /// current backstop token.
    fn queue_swap_backstop(e: Env, new_backstop: Address, new_backstop_token: Address);

    /// Fetch the queued backstop swap, or None if nothing is queued.
    fn get_queued_swap(e: Env) -> Option<backstop_manager::Swap>;

    /// Verifies that a queued swap still meets the requirements to be executed. Expired
    /// swaps may always be cancelled. If cancelled, the swap must be recreated.
    ///
    /// ### Errors
    /// If the queued swap is still valid and unexpired.
    fn cancel_swap_backstop(e: Env);

    /// Executes a queued swap of the listed backstop module to one with more effective backstop deposits
    ///
    /// ### Errors
    /// If the input contract does not have more backstop deposits than the listed backstop module,
    /// if the queued swap has not been unlocked, or if its seven-day execution window expired.
    fn swap_backstop(e: Env);

    /// (Backstop only) Distributes an initial BLNT allocation after a new backstop is set
    ///
    /// ### Arguments
    /// * `list` - The list of address and amounts to distribute too
    ///
    /// ### Errors
    /// If drop has already been called for the backstop, the backstop is not the caller,
    /// or the list exceeds the drop amount maximum.
    fn drop(e: Env, list: Vec<(Address, i128)>);
}

#[contractimpl]
impl EmitterContract {
    pub fn __constructor(e: Env, initializer: Address) {
        storage::set_initializer(&e, &initializer);
        storage::extend_instance(&e);
    }
}

#[contractimpl]
impl Emitter for EmitterContract {
    fn initialize(e: Env, blnd_token: Address, backstop: Address, backstop_token: Address) {
        storage::extend_instance(&e);
        if storage::get_is_init(&e) {
            panic_with_error!(&e, EmitterError::AlreadyInitializedError)
        }
        storage::get_initializer(&e).require_auth();
        let emitter = e.current_contract_address();
        if blnd_token.executable() != Some(Executable::StellarAsset)
            || TokenClient::new(&e, &blnd_token).decimals() != 7
            || StellarAssetClient::new(&e, &blnd_token).admin() != emitter
        {
            panic_with_error!(&e, EmitterError::InvalidEmissionToken);
        }

        storage::set_emission_token(&e, &blnd_token);
        storage::set_backstop(&e, &backstop);
        storage::set_initial_backstop(&e, &backstop);
        storage::set_backstop_token(&e, &backstop_token);
        storage::set_last_distro_time(&e, &backstop, e.ledger().timestamp());

        storage::remove_initializer(&e);
        storage::set_is_init(&e);
    }

    fn distribute(e: Env) -> i128 {
        storage::extend_instance(&e);
        let backstop_address = storage::get_backstop(&e);
        backstop_address.require_auth();

        let distribution_amount = emitter::execute_distribute(&e, &backstop_address);

        e.events().publish(
            (Symbol::new(&e, "distribute"),),
            (backstop_address, distribution_amount),
        );
        distribution_amount
    }

    fn get_last_distro(e: Env, backstop_id: Address) -> u64 {
        storage::get_last_distro_time(&e, &backstop_id)
    }

    fn get_backstop(e: Env) -> Address {
        storage::get_backstop(&e)
    }

    fn queue_swap_backstop(e: Env, new_backstop: Address, new_backstop_token: Address) {
        storage::extend_instance(&e);
        let swap =
            backstop_manager::execute_queue_swap_backstop(&e, &new_backstop, &new_backstop_token);

        e.events().publish((Symbol::new(&e, "q_swap"),), swap);
    }

    fn get_queued_swap(e: Env) -> Option<backstop_manager::Swap> {
        storage::get_queued_swap(&e)
    }

    fn cancel_swap_backstop(e: Env) {
        storage::extend_instance(&e);
        let swap = backstop_manager::execute_cancel_swap_backstop(&e);

        e.events().publish((Symbol::new(&e, "del_swap"),), swap);
    }

    fn swap_backstop(e: Env) {
        storage::extend_instance(&e);
        let swap = backstop_manager::execute_swap_backstop(&e);

        e.events().publish((Symbol::new(&e, "swap"),), swap);
    }

    fn drop(e: Env, list: Vec<(Address, i128)>) {
        storage::extend_instance(&e);
        emitter::execute_drop(&e, &list);

        e.events().publish((Symbol::new(&e, "drop"),), list);
    }
}

#[cfg(test)]
mod tests {
    use super::{EmitterClient, EmitterContract};
    use soroban_sdk::{
        contract, contractimpl,
        testutils::{Address as _, Ledger, MockAuth, MockAuthInvoke},
        Address, Env, IntoVal, Map,
    };

    #[contract]
    struct DistributionBackstop;

    #[contractimpl]
    impl DistributionBackstop {
        pub fn distribute(e: Env, emitter: Address) -> i128 {
            EmitterClient::new(&e, &emitter).distribute()
        }
    }

    #[contract]
    struct ComparisonToken;

    #[contractimpl]
    impl ComparisonToken {
        pub fn set_balance(e: Env, address: Address, amount: i128) {
            let mut balances: Map<Address, i128> =
                e.storage().instance().get(&0_u32).unwrap_or(Map::new(&e));
            balances.set(address, amount);
            e.storage().instance().set(&0_u32, &balances);
        }

        pub fn balance(e: Env, address: Address) -> i128 {
            e.storage()
                .instance()
                .get::<_, Map<Address, i128>>(&0_u32)
                .and_then(|balances| balances.get(address))
                .unwrap_or(0)
        }
    }

    #[test]
    fn initialize_requires_the_constructor_bound_authority() {
        let e = Env::default();
        let initializer = Address::generate(&e);
        let emitter = e.register(EmitterContract, (&initializer,));
        let blnt = e
            .register_stellar_asset_contract_v2(emitter.clone())
            .address();
        let backstop = Address::generate(&e);
        let backstop_token = Address::generate(&e);
        let client = EmitterClient::new(&e, &emitter);

        assert!(client
            .try_initialize(&blnt, &backstop, &backstop_token)
            .is_err());

        e.mock_all_auths();
        client.initialize(&blnt, &backstop, &backstop_token);
        assert_eq!(client.get_backstop(), backstop);
        assert!(client
            .try_initialize(&blnt, &Address::generate(&e), &backstop_token)
            .is_err());
    }

    #[test]
    fn initialization_rejects_an_invalid_emission_token() {
        let e = Env::default();
        e.mock_all_auths();
        let initializer = Address::generate(&e);
        let emitter = e.register(EmitterContract, (&initializer,));
        let wrong_admin = Address::generate(&e);
        let wrong_admin_token = e.register_stellar_asset_contract_v2(wrong_admin).address();
        let client = EmitterClient::new(&e, &emitter);

        assert!(client
            .try_initialize(
                &wrong_admin_token,
                &Address::generate(&e),
                &Address::generate(&e),
            )
            .is_err());
        assert!(client
            .try_initialize(
                &Address::generate(&e),
                &Address::generate(&e),
                &Address::generate(&e),
            )
            .is_err());
    }

    #[test]
    fn distribution_requires_the_current_backstop() {
        let e = Env::default();
        let initializer = Address::generate(&e);
        let emitter = e.register(EmitterContract, (&initializer,));
        let blnt = e
            .register_stellar_asset_contract_v2(emitter.clone())
            .address();
        let backstop = e.register(DistributionBackstop, ());
        let backstop_token = Address::generate(&e);
        let client = EmitterClient::new(&e, &emitter);

        client
            .mock_auths(&[MockAuth {
                address: &initializer,
                invoke: &MockAuthInvoke {
                    contract: &emitter,
                    fn_name: "initialize",
                    args: (&blnt, &backstop, &backstop_token).into_val(&e),
                    sub_invokes: &[],
                },
            }])
            .initialize(&blnt, &backstop, &backstop_token);
        e.ledger().set_timestamp(100);

        assert!(client.try_distribute().is_err());
        assert_eq!(client.get_last_distro(&backstop), 0);
        assert_eq!(
            DistributionBackstopClient::new(&e, &backstop).distribute(&emitter),
            100 * crate::constants::SCALAR_7
        );
        assert_eq!(client.get_last_distro(&backstop), 100);
        e.ledger().set_timestamp(110);
        assert!(client.try_distribute().is_err());
        assert_eq!(client.get_last_distro(&backstop), 100);
    }

    #[test]
    fn replacement_remains_permissionless_without_recipient_distribution() {
        const QUEUE_SECONDS: u64 = 31 * 24 * 60 * 60;

        let e = Env::default();
        let initializer = Address::generate(&e);
        let emitter = e.register(EmitterContract, (&initializer,));
        let blnt = e
            .register_stellar_asset_contract_v2(emitter.clone())
            .address();
        let comparison_token = e.register(ComparisonToken, ());
        let comparison = ComparisonTokenClient::new(&e, &comparison_token);
        let incumbent = Address::generate(&e);
        let first_candidate = Address::generate(&e);
        let second_candidate = Address::generate(&e);
        let client = EmitterClient::new(&e, &emitter);

        client
            .mock_auths(&[MockAuth {
                address: &initializer,
                invoke: &MockAuthInvoke {
                    contract: &emitter,
                    fn_name: "initialize",
                    args: (&blnt, &incumbent, &comparison_token).into_val(&e),
                    sub_invokes: &[],
                },
            }])
            .initialize(&blnt, &incumbent, &comparison_token);
        comparison.set_balance(&incumbent, &1);
        comparison.set_balance(&first_candidate, &2);
        comparison.set_balance(&second_candidate, &3);

        client.queue_swap_backstop(&first_candidate, &comparison_token);
        e.ledger().set_timestamp(QUEUE_SECONDS);
        client.swap_backstop();
        assert_eq!(client.get_backstop(), first_candidate);

        // The first candidate never calls `distribute`, but that does not gate
        // the established queue and replacement path.
        client.queue_swap_backstop(&second_candidate, &comparison_token);
        e.ledger().set_timestamp(2 * QUEUE_SECONDS);
        client.swap_backstop();
        assert_eq!(client.get_backstop(), second_candidate);
    }
}
