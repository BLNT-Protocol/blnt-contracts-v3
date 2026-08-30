use soroban_sdk::{panic_with_error, Address, Env, Vec};

use crate::{
    dependencies::AccessControllerClient,
    pool::{Request, RequestType},
    storage, PoolError,
};

pub const RESERVE_SUPPLY_ALLOWED: u32 = 1 << 0;
pub const RESERVE_BORROW_ALLOWED: u32 = 1 << 1;

/// Require the position owner to hold every requested permission. An open
/// pool has no controller and preserves the inherited permissionless behavior.
pub fn require_permissions(e: &Env, user: &Address, required: u32) {
    if required == 0 {
        return;
    }
    if let Some(controller) = storage::get_access_controller(e) {
        let permissions = AccessControllerClient::new(e, &controller)
            .permissions(&e.current_contract_address(), user);
        if permissions & required != required {
            panic_with_error!(e, PoolError::UnauthorizedError);
        }
    }
}

/// Require an affirmative controller response proving that `permission` is
/// absent. Permissionless pools cannot authorize forced exits.
pub fn require_permission_absent(e: &Env, user: &Address, permission: u32) {
    let controller = storage::get_access_controller(e)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::UnauthorizedError));
    let permissions = AccessControllerClient::new(e, &controller)
        .permissions(&e.current_contract_address(), user);
    if permissions & permission != 0 {
        panic_with_error!(e, PoolError::UnauthorizedError);
    }
}

/// Return the union of permissions required by the request types. Permission
/// checks deliberately use request semantics rather than net token flow.
pub fn request_permissions(e: &Env, requests: &Vec<Request>) -> u32 {
    let mut required = 0;
    for request in requests.iter() {
        match RequestType::from_u32(e, request.request_type) {
            RequestType::Supply => required |= RESERVE_SUPPLY_ALLOWED,
            RequestType::SupplyCollateral | RequestType::FillUserLiquidationAuction => {
                required |= RESERVE_SUPPLY_ALLOWED | RESERVE_BORROW_ALLOWED;
            }
            RequestType::Borrow => required |= RESERVE_BORROW_ALLOWED,
            RequestType::Withdraw
            | RequestType::WithdrawCollateral
            | RequestType::Repay
            | RequestType::FillBadDebtAuction
            | RequestType::FillInterestAuction
            | RequestType::FillProtocolFeeAuction
            | RequestType::DeleteLiquidationAuction => {}
        }
    }
    required
}

#[cfg(test)]
mod tests {
    use soroban_sdk::{testutils::Address as _, vec, Address, Env};

    use crate::testutils::{
        create_pool, create_pool_with_access_controller, MockAccessController,
        MockAccessControllerClient,
    };

    use super::*;

    fn permissioned_pool(e: &Env) -> (Address, MockAccessControllerClient<'_>) {
        let controller = e.register(MockAccessController, ());
        let pool = create_pool_with_access_controller(e, Some(controller.clone()));
        (pool, MockAccessControllerClient::new(e, &controller))
    }

    #[test]
    fn open_pool_preserves_permissionless_access() {
        let e = Env::default();
        let pool = create_pool(&e);
        let user = Address::generate(&e);
        e.as_contract(&pool, || {
            require_permissions(&e, &user, RESERVE_SUPPLY_ALLOWED | RESERVE_BORROW_ALLOWED);
        });
    }

    #[test]
    fn controller_permissions_are_pool_and_user_specific() {
        let e = Env::default();
        let (pool, controller) = permissioned_pool(&e);
        let user = Address::generate(&e);
        controller.set_permissions(&pool, &user, &RESERVE_SUPPLY_ALLOWED);

        e.as_contract(&pool, || {
            require_permissions(&e, &user, RESERVE_SUPPLY_ALLOWED);
            require_permission_absent(&e, &user, RESERVE_BORROW_ALLOWED);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #4)")]
    fn missing_permission_fails() {
        let e = Env::default();
        let (pool, controller) = permissioned_pool(&e);
        let user = Address::generate(&e);
        controller.set_permissions(&pool, &user, &RESERVE_SUPPLY_ALLOWED);

        e.as_contract(&pool, || {
            require_permissions(&e, &user, RESERVE_BORROW_ALLOWED);
        });
    }

    #[test]
    #[should_panic]
    fn controller_failure_is_not_treated_as_revocation() {
        let e = Env::default();
        let (pool, controller) = permissioned_pool(&e);
        let user = Address::generate(&e);
        controller.set_fail(&true);

        e.as_contract(&pool, || {
            require_permission_absent(&e, &user, RESERVE_BORROW_ALLOWED);
        });
    }

    #[test]
    fn request_bits_follow_action_semantics() {
        let e = Env::default();
        let asset = Address::generate(&e);
        let requests = vec![
            &e,
            Request {
                request_type: RequestType::Repay as u32,
                address: asset.clone(),
                amount: 1,
            },
            Request {
                request_type: RequestType::SupplyCollateral as u32,
                address: asset,
                amount: 1,
            },
        ];
        assert_eq!(
            request_permissions(&e, &requests),
            RESERVE_SUPPLY_ALLOWED | RESERVE_BORROW_ALLOWED
        );
    }
}
