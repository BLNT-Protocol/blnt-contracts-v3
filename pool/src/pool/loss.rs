use soroban_sdk::{
    contracttype, panic_with_error, unwrap::UnwrapOptimized, Address, BytesN, Env, Map, Symbol,
};

use crate::{storage, PoolError};

use super::Positions;

const LOSS_RECORDS_KEY: &str = "LossRec";
const MAX_LOSS_RECORDS_PER_KIND: u32 = 30;

/// Canonical nonzero records that can prevent backstop withdrawals.
///
/// Values remain in each reserve, auction, or bad-debt record's own units.
/// They are never summed across maps.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
struct BackstopLossRecords {
    committed_losses: Map<BytesN<32>, bool>,
    liabilities: Map<Address, i128>,
    unresolved_bad_debt: Map<Address, i128>,
}

/// Counts derived from the canonical records that prevent backstop withdrawals.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct BackstopLossState {
    pub committed_loss_entries: u32,
    pub liability_entries: u32,
    pub unresolved_bad_debt_entries: u32,
}

impl BackstopLossState {
    pub(crate) fn is_clear(&self) -> bool {
        self.committed_loss_entries == 0
            && self.liability_entries == 0
            && self.unresolved_bad_debt_entries == 0
    }
}

pub(crate) fn initialize_loss_records(e: &Env) {
    set_loss_records(e, &empty_loss_records(e));
}

pub(crate) fn backstop_loss_state(e: &Env) -> BackstopLossState {
    let records = get_loss_records(e);
    let committed_loss_entries = records
        .committed_losses
        .len()
        .checked_add(u32::from(crate::auctions::has_prepared_bad_debt_auction(e)))
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    BackstopLossState {
        committed_loss_entries,
        liability_entries: records.liabilities.len(),
        unresolved_bad_debt_entries: records.unresolved_bad_debt.len(),
    }
}

pub(crate) fn backstop_liability(e: &Env, reserve: &Address) -> i128 {
    get_loss_records(e)
        .liabilities
        .get(reserve.clone())
        .unwrap_or(0)
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
            require_record_capacity(
                e,
                liabilities.len(),
                liabilities.contains_key(reserve.clone()),
            );
            liabilities.set(reserve, amount);
        }
    }

    let mut records = get_loss_records(e);
    records.liabilities = liabilities;
    set_loss_records(e, &records);
}

#[allow(dead_code)]
pub(crate) fn set_unresolved_bad_debt(e: &Env, reserve: &Address, amount: i128) {
    let mut records = get_loss_records(e);
    set_nonzero_address_record(e, &mut records.unresolved_bad_debt, reserve, amount);
    set_loss_records(e, &records);
}

#[allow(dead_code)]
pub(crate) fn set_committed_loss(e: &Env, auction: &BytesN<32>, committed: bool) {
    let mut records = get_loss_records(e);
    if committed {
        require_record_capacity(
            e,
            records.committed_losses.len(),
            records.committed_losses.contains_key(auction.clone()),
        );
        records.committed_losses.set(auction.clone(), true);
    } else {
        records.committed_losses.remove(auction.clone());
    }
    set_loss_records(e, &records);
}

fn empty_loss_records(e: &Env) -> BackstopLossRecords {
    BackstopLossRecords {
        committed_losses: Map::new(e),
        liabilities: Map::new(e),
        unresolved_bad_debt: Map::new(e),
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

fn set_nonzero_address_record(
    e: &Env,
    records: &mut Map<Address, i128>,
    key: &Address,
    amount: i128,
) {
    require_valid_loss_amount(e, amount);
    if amount == 0 {
        records.remove(key.clone());
    } else {
        require_record_capacity(e, records.len(), records.contains_key(key.clone()));
        records.set(key.clone(), amount);
    }
}

fn require_valid_loss_amount(e: &Env, amount: i128) {
    if amount < 0 {
        panic_with_error!(e, PoolError::InvalidLossAmount);
    }
}

fn require_record_capacity(e: &Env, len: u32, exists: bool) {
    if !exists && len >= MAX_LOSS_RECORDS_PER_KIND {
        panic_with_error!(e, PoolError::TooManyLossRecords);
    }
}

#[cfg(test)]
mod tests {
    use soroban_sdk::{testutils::Address as _, Address, BytesN};

    use crate::{storage, testutils::create_pool, PoolClient};

    use super::*;

    #[test]
    fn loss_state_tracks_liabilities_and_future_loss_records() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        let pool = create_pool(&e);
        let client = PoolClient::new(&e, &pool);
        let backstop = e.as_contract(&pool, || storage::get_backstop(&e));
        let reserve = Address::generate(&e);
        let auction = BytesN::from_array(&e, &[7; 32]);

        assert_eq!(
            client.backstop_loss_state(),
            BackstopLossState {
                committed_loss_entries: 0,
                liability_entries: 0,
                unresolved_bad_debt_entries: 0,
            }
        );
        assert!(client.backstop_withdrawal_allowed(&backstop));

        e.as_contract(&pool, || {
            storage::push_res_list(&e, &reserve);
            let mut positions = Positions::env_default(&e);
            positions.liabilities.set(0, 100);
            storage::set_user_positions(&e, &backstop, &positions);
        });
        assert_eq!(client.backstop_loss_state().liability_entries, 1);
        assert!(!client.backstop_withdrawal_allowed(&backstop));

        e.as_contract(&pool, || {
            storage::set_user_positions(&e, &backstop, &Positions::env_default(&e));
            set_unresolved_bad_debt(&e, &reserve, 50);
        });
        assert_eq!(
            client.backstop_loss_state(),
            BackstopLossState {
                committed_loss_entries: 0,
                liability_entries: 0,
                unresolved_bad_debt_entries: 1,
            }
        );

        e.as_contract(&pool, || {
            set_unresolved_bad_debt(&e, &reserve, 0);
            set_committed_loss(&e, &auction, true);
        });
        assert_eq!(client.backstop_loss_state().committed_loss_entries, 1);

        e.as_contract(&pool, || set_committed_loss(&e, &auction, false));
        assert!(client.backstop_withdrawal_allowed(&backstop));
    }

    #[test]
    fn updating_existing_records_preserves_counts() {
        let e = Env::default();
        let pool = create_pool(&e);
        let reserve = Address::generate(&e);

        e.as_contract(&pool, || {
            set_unresolved_bad_debt(&e, &reserve, 100);
            set_unresolved_bad_debt(&e, &reserve, 40);
        });

        assert_eq!(
            PoolClient::new(&e, &pool)
                .backstop_loss_state()
                .unresolved_bad_debt_entries,
            1
        );
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1225)")]
    fn canonical_loss_records_reject_negative_amounts() {
        let e = Env::default();
        let pool = create_pool(&e);
        let reserve = Address::generate(&e);

        e.as_contract(&pool, || set_unresolved_bad_debt(&e, &reserve, -1));
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1226)")]
    fn canonical_loss_records_are_bounded() {
        let e = Env::default();
        let pool = create_pool(&e);

        e.as_contract(&pool, || {
            for _ in 0..MAX_LOSS_RECORDS_PER_KIND {
                set_unresolved_bad_debt(&e, &Address::generate(&e), 1);
            }
            set_unresolved_bad_debt(&e, &Address::generate(&e), 1);
        });
    }

    #[test]
    fn missing_loss_records_fail_closed() {
        let e = Env::default();
        let pool = create_pool(&e);
        let client = PoolClient::new(&e, &pool);
        let backstop = e.as_contract(&pool, || {
            let backstop = storage::get_backstop(&e);
            e.storage()
                .instance()
                .remove(&Symbol::new(&e, LOSS_RECORDS_KEY));
            backstop
        });

        assert!(client.try_backstop_withdrawal_allowed(&backstop).is_err());
    }
}
