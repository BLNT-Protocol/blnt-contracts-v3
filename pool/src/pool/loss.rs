use soroban_sdk::{
    contracttype, panic_with_error, unwrap::UnwrapOptimized, Address, Env, Map, Symbol,
};

use crate::{storage, PoolError};

use super::Positions;

const LOSS_RECORDS_KEY: &str = "LossRec";

/// Reserve-addressed mirror of the configured backstop's dToken positions.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
struct BackstopLossRecords {
    liabilities: Map<Address, i128>,
}

pub(crate) fn initialize_loss_records(e: &Env) {
    set_loss_records(e, &empty_loss_records(e));
}

pub(crate) fn backstop_liability(e: &Env, reserve: &Address) -> i128 {
    get_loss_records(e)
        .liabilities
        .get(reserve.clone())
        .unwrap_or(0)
}

pub(crate) fn backstop_liabilities(e: &Env) -> Map<Address, i128> {
    get_loss_records(e).liabilities
}

/// Mirror the configured backstop's existing v2 dToken positions into
/// reserve-addressed canonical loss records.
pub(crate) fn sync_backstop_liabilities(e: &Env, positions: &Positions) {
    let reserves = storage::get_res_list(e);
    let mut liabilities = Map::<Address, i128>::new(e);
    for (reserve_index, amount) in positions.liabilities.iter() {
        require_valid_loss_amount(e, amount);
        if amount > 0 {
            let reserve = reserves
                .get(reserve_index)
                .unwrap_or_else(|| panic_with_error!(e, PoolError::InternalReserveNotFound));
            liabilities.set(reserve, amount);
        }
    }

    let mut records = get_loss_records(e);
    records.liabilities = liabilities;
    set_loss_records(e, &records);
}

fn empty_loss_records(e: &Env) -> BackstopLossRecords {
    BackstopLossRecords {
        liabilities: Map::new(e),
    }
}

fn get_loss_records(e: &Env) -> BackstopLossRecords {
    e.storage()
        .instance()
        .get::<Symbol, BackstopLossRecords>(&Symbol::new(e, LOSS_RECORDS_KEY))
        .unwrap_optimized()
}

fn set_loss_records(e: &Env, records: &BackstopLossRecords) {
    e.storage()
        .instance()
        .set::<Symbol, BackstopLossRecords>(&Symbol::new(e, LOSS_RECORDS_KEY), records);
}

fn require_valid_loss_amount(e: &Env, amount: i128) {
    if amount < 0 {
        panic_with_error!(e, PoolError::InvalidLossAmount);
    }
}

#[cfg(test)]
mod tests {
    use soroban_sdk::{map, testutils::Address as _, Address};

    use crate::{storage, testutils::create_pool};

    use super::*;

    #[test]
    fn liability_mirror_tracks_backstop_positions() {
        let e = Env::default();
        let pool = create_pool(&e);
        let backstop = e.as_contract(&pool, || storage::get_backstop(&e));
        let reserve = Address::generate(&e);

        e.as_contract(&pool, || {
            storage::push_res_list(&e, &reserve);
            let mut positions = Positions::env_default(&e);
            positions.liabilities.set(0, 100);
            storage::set_user_positions(&e, &backstop, &positions);
            assert_eq!(backstop_liabilities(&e), map![&e, (reserve.clone(), 100)]);
        });

        e.as_contract(&pool, || {
            storage::set_user_positions(&e, &backstop, &Positions::env_default(&e));
            assert!(backstop_liabilities(&e).is_empty());
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1225)")]
    fn liability_mirror_rejects_negative_amounts() {
        let e = Env::default();
        let pool = create_pool(&e);

        e.as_contract(&pool, || {
            storage::push_res_list(&e, &Address::generate(&e));
            let mut positions = Positions::env_default(&e);
            positions.liabilities.set(0, -1);
            sync_backstop_liabilities(&e, &positions);
        });
    }

}
