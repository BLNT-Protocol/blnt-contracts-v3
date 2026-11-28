use crate::{
    constants::{MAX_INITIAL_DROP, MAX_MIGRATION_DROP, SCALAR_7},
    errors::EmitterError,
    storage,
};
use sep_41_token::StellarAssetClient;
use soroban_sdk::{panic_with_error, Address, Env, Vec};

/// Perform a distribution
pub fn execute_distribute(e: &Env, backstop: &Address) -> i128 {
    let timestamp = e.ledger().timestamp();
    let seconds_since_last_distro = timestamp - storage::get_last_distro_time(e, backstop);
    // BLNT is distributed at a rate of 1 token per second.
    let distribution_amount = (seconds_since_last_distro as i128) * SCALAR_7;
    storage::set_last_distro_time(e, backstop, timestamp);

    let blnt_id = storage::get_emission_token(e);
    let blnt_client = StellarAssetClient::new(e, &blnt_id);
    blnt_client.mint(backstop, &distribution_amount);

    distribution_amount
}

/// Perform the one-time initial BLNT distribution.
pub fn execute_drop(e: &Env, list: &Vec<(Address, i128)>) {
    let backstop = storage::get_backstop(e);
    backstop.require_auth();

    if storage::get_drop_status(e, &backstop) {
        panic_with_error!(e, EmitterError::BadDrop);
    }
    let max_drop = if backstop == storage::get_initial_backstop(e) {
        MAX_INITIAL_DROP
    } else {
        MAX_MIGRATION_DROP
    };

    let mut drop_amount = 0_i128;
    for (_, amt) in list.iter() {
        if amt.is_negative() {
            panic_with_error!(e, EmitterError::BadDrop);
        }
        drop_amount = drop_amount
            .checked_add(amt)
            .unwrap_or_else(|| panic_with_error!(e, EmitterError::OverflowError));
        if drop_amount > max_drop {
            panic_with_error!(e, EmitterError::BadDrop);
        }
    }

    let blnt_id = storage::get_emission_token(e);
    let blnt_client = StellarAssetClient::new(e, &blnt_id);
    for (addr, amt) in list.iter() {
        blnt_client.mint(&addr, &amt);
    }
    storage::set_drop_status(e, &backstop);
}

#[cfg(test)]
mod tests {

    use crate::{storage, testutils::create_emitter};

    use super::*;
    use sep_41_token::testutils::MockTokenClient;
    use soroban_sdk::{
        testutils::{Address as _, Ledger, LedgerInfo},
        vec,
    };

    #[test]
    fn test_distribute() {
        let e = Env::default();
        e.mock_all_auths();

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 50,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let emitter = create_emitter(&e);
        let backstop = Address::generate(&e);

        let blnt_id = e
            .register_stellar_asset_contract_v2(emitter.clone())
            .address();
        let blnt_client = MockTokenClient::new(&e, &blnt_id);

        e.as_contract(&emitter, || {
            storage::set_last_distro_time(&e, &backstop, 1000);
            storage::set_backstop(&e, &backstop);
            storage::set_emission_token(&e, &blnt_id);

            let result = execute_distribute(&e, &backstop);
            assert_eq!(result, 11345_0000000);
            assert_eq!(blnt_client.balance(&backstop), 11345_0000000);
            assert_eq!(storage::get_last_distro_time(&e, &backstop), 12345);
        });
    }

    #[test]
    fn test_drop() {
        let e = Env::default();
        e.mock_all_auths();

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 5000000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let frodo = Address::generate(&e);
        let samwise = Address::generate(&e);
        let emitter = create_emitter(&e);
        let backstop = Address::generate(&e);

        let blnt_id = e
            .register_stellar_asset_contract_v2(emitter.clone())
            .address();
        let blnt_client = MockTokenClient::new(&e, &blnt_id);
        let drop_list = vec![
            &e,
            (frodo.clone(), 120_000_000 * SCALAR_7),
            (samwise.clone(), 30_000_000 * SCALAR_7),
        ];

        e.as_contract(&emitter, || {
            storage::set_last_distro_time(&e, &backstop, 1000);
            storage::set_backstop(&e, &backstop);
            storage::set_initial_backstop(&e, &backstop);
            storage::set_emission_token(&e, &blnt_id);

            execute_drop(&e, &drop_list);
            assert!(storage::get_drop_status(&e, &backstop));
            assert_eq!(blnt_client.balance(&frodo), 120_000_000 * SCALAR_7);
            assert_eq!(blnt_client.balance(&samwise), 30_000_000 * SCALAR_7);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1101)")]
    fn test_drop_already_dropped() {
        let e = Env::default();
        e.mock_all_auths();

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 5000000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let frodo = Address::generate(&e);
        let samwise = Address::generate(&e);
        let emitter = create_emitter(&e);
        let backstop = Address::generate(&e);

        let blnt_id = e
            .register_stellar_asset_contract_v2(emitter.clone())
            .address();
        let drop_list = vec![
            &e,
            (frodo.clone(), 20_000_000 * SCALAR_7),
            (samwise.clone(), 30_000_000 * SCALAR_7),
        ];

        e.as_contract(&emitter, || {
            storage::set_last_distro_time(&e, &backstop, 1000);
            storage::set_backstop(&e, &backstop);
            storage::set_emission_token(&e, &blnt_id);
            storage::set_drop_status(&e, &backstop);

            execute_drop(&e, &drop_list);
            assert!(storage::get_drop_status(&e, &backstop));
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1101)")]
    fn test_drop_too_large() {
        let e = Env::default();
        e.mock_all_auths();

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 5000000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let frodo = Address::generate(&e);
        let samwise = Address::generate(&e);
        let emitter = create_emitter(&e);
        let backstop = Address::generate(&e);

        let blnt_id = e
            .register_stellar_asset_contract_v2(emitter.clone())
            .address();
        let drop_list = vec![
            &e,
            (frodo.clone(), 120_000_000 * SCALAR_7),
            (samwise.clone(), 30_000_001 * SCALAR_7),
        ];

        e.as_contract(&emitter, || {
            storage::set_last_distro_time(&e, &backstop, 1000);
            storage::set_backstop(&e, &backstop);
            storage::set_initial_backstop(&e, &backstop);
            storage::set_emission_token(&e, &blnt_id);

            execute_drop(&e, &drop_list);
            assert!(!storage::get_drop_status(&e, &backstop));
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1101)")]
    fn test_migration_drop_retains_fifty_million_cap() {
        let e = Env::default();
        e.mock_all_auths();

        let emitter = create_emitter(&e);
        let initial_backstop = Address::generate(&e);
        let migration_backstop = Address::generate(&e);
        let recipient = Address::generate(&e);
        let blnt_id = e
            .register_stellar_asset_contract_v2(emitter.clone())
            .address();
        let drop_list = vec![&e, (recipient, 50_000_000 * SCALAR_7 + 1)];

        e.as_contract(&emitter, || {
            storage::set_backstop(&e, &migration_backstop);
            storage::set_initial_backstop(&e, &initial_backstop);
            storage::set_emission_token(&e, &blnt_id);
            execute_drop(&e, &drop_list);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1101)")]
    fn test_drop_rejects_negative_amount() {
        let e = Env::default();
        e.mock_all_auths();

        let emitter = create_emitter(&e);
        let backstop = Address::generate(&e);
        let recipient = Address::generate(&e);
        let blnt_id = e
            .register_stellar_asset_contract_v2(emitter.clone())
            .address();
        let drop_list = vec![&e, (recipient, -1)];

        e.as_contract(&emitter, || {
            storage::set_backstop(&e, &backstop);
            storage::set_initial_backstop(&e, &backstop);
            storage::set_emission_token(&e, &blnt_id);
            execute_drop(&e, &drop_list);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1101)")]
    fn test_drop_negative_amount_cannot_bypass_cap() {
        let e = Env::default();
        e.mock_all_auths();

        let emitter = create_emitter(&e);
        let backstop = Address::generate(&e);
        let frodo = Address::generate(&e);
        let samwise = Address::generate(&e);
        let blnt_id = e
            .register_stellar_asset_contract_v2(emitter.clone())
            .address();
        let drop_list = vec![&e, (frodo, 150_000_001 * SCALAR_7), (samwise, -SCALAR_7)];

        e.as_contract(&emitter, || {
            storage::set_backstop(&e, &backstop);
            storage::set_initial_backstop(&e, &backstop);
            storage::set_emission_token(&e, &blnt_id);
            execute_drop(&e, &drop_list);
        });
    }
}
