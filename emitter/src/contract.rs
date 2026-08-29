// Preserve the legacy event topic and data encoding rather than migrating it to
// the SDK 27 typed-event API.
#![allow(deprecated)]

use crate::{backstop_manager, conversion, emitter, errors::EmitterError, storage};
use sep_41_token::{StellarAssetClient, TokenClient};
use soroban_sdk::{
    contract, contractclient, contractimpl, panic_with_error, Address, Env, Executable, Symbol, Vec,
};

/// ### Emitter
///
/// Emits BLNT to the v3 backstop and offers a time-limited legacy BLND conversion.
#[contract]
pub struct EmitterContract;

pub const SWAP_WINDOW_SECONDS: u64 = 60 * 24 * 60 * 60;

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
    /// Returns the amount of BLNT tokens distributed
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

    /// Verifies that a queued swap still meets the requirements to be executed. If not,
    /// the queued swap is cancelled and must be recreated.
    ///
    /// ### Errors
    /// If the queued swap is still valid.
    fn cancel_swap_backstop(e: Env);

    /// Executes a queued swap of the listed backstop module to one with more effective backstop deposits
    ///
    /// ### Errors
    /// If the input contract does not have more backstop deposits than the listed backstop module,
    /// or if the queued swap has not been unlocked.
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

    /// Irreversibly exchange legacy BLND for the configured emission token.
    ///
    /// The exchange is available only during the first 60 days after this
    /// emitter contract is instantiated.
    fn swap_blnd_for_blnt(e: Env, from: Address, to: Address, amount: i128) -> i128;

    /// Fetch the immutable exclusive BLND-to-BLNT swap deadline.
    fn get_swap_deadline(e: Env) -> u64;

    /// Fetch the legacy BLND token accepted by the swap.
    fn get_legacy_blnd_token(e: Env) -> Address;

    /// Fetch the cumulative BLND burned through the swap.
    fn get_total_swapped(e: Env) -> i128;
}

#[contractimpl]
impl EmitterContract {
    pub fn __constructor(e: Env, legacy_blnd_token: Address, initializer: Address) {
        storage::set_legacy_blnd_token(&e, &legacy_blnd_token);
        storage::set_initializer(&e, &initializer);
        let deadline = e
            .ledger()
            .timestamp()
            .checked_add(SWAP_WINDOW_SECONDS)
            .unwrap_or_else(|| panic_with_error!(&e, EmitterError::OverflowError));
        storage::set_swap_deadline(&e, deadline);
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
        let legacy_blnd_token = storage::get_legacy_blnd_token(&e);
        let emitter = e.current_contract_address();
        if blnd_token == legacy_blnd_token
            || blnd_token.executable() != Some(Executable::StellarAsset)
            || legacy_blnd_token.executable() != Some(Executable::StellarAsset)
            || TokenClient::new(&e, &blnd_token).decimals() != 7
            || TokenClient::new(&e, &legacy_blnd_token).decimals() != 7
            || StellarAssetClient::new(&e, &blnd_token).admin() != emitter
        {
            panic_with_error!(&e, EmitterError::InvalidSwapToken);
        }

        storage::set_emission_token(&e, &blnd_token);
        storage::set_backstop(&e, &backstop);
        storage::set_backstop_token(&e, &backstop_token);
        storage::set_last_distro_time(&e, &backstop, e.ledger().timestamp());

        storage::remove_initializer(&e);
        storage::set_is_init(&e);
    }

    fn distribute(e: Env) -> i128 {
        storage::extend_instance(&e);
        let backstop_address = storage::get_backstop(&e);

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

    fn swap_blnd_for_blnt(e: Env, from: Address, to: Address, amount: i128) -> i128 {
        storage::extend_instance(&e);
        let total = conversion::execute_swap_blnd_for_blnt(&e, &from, &to, amount);
        e.events()
            .publish((Symbol::new(&e, "swap_blnd"), from, to), (amount, total));
        total
    }

    fn get_swap_deadline(e: Env) -> u64 {
        storage::get_swap_deadline(&e)
    }

    fn get_legacy_blnd_token(e: Env) -> Address {
        storage::get_legacy_blnd_token(&e)
    }

    fn get_total_swapped(e: Env) -> i128 {
        storage::get_total_swapped(&e)
    }
}

#[cfg(test)]
mod tests {
    use super::{EmitterClient, EmitterContract};
    use soroban_sdk::{testutils::Address as _, Address, Env};

    #[test]
    fn initialize_requires_the_constructor_bound_authority() {
        let e = Env::default();
        let initializer = Address::generate(&e);
        let legacy_admin = Address::generate(&e);
        let legacy_blnd = e.register_stellar_asset_contract_v2(legacy_admin).address();
        let emitter = e.register(EmitterContract, (&legacy_blnd, &initializer));
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
}
