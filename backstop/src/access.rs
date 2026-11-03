use soroban_sdk::{panic_with_error, Address, Env};

use crate::{
    dependencies::{AccessControllerClient, PoolFactoryClient},
    storage, BackstopError,
};

pub const BACKSTOP_DEPOSIT_ALLOWED: u32 = 1 << 2;

fn pool_access_controller(e: &Env, pool: &Address) -> Option<Address> {
    let factory = PoolFactoryClient::new(e, &storage::get_pool_factory(e));
    if !factory.is_pool(pool) {
        panic_with_error!(e, BackstopError::NotPool);
    }
    factory.backstop_config(pool).access_controller
}

/// Require permission to create or restore active backstop shares. Pools
/// without a controller preserve the inherited permissionless behavior.
pub fn require_deposit_permission(e: &Env, pool: &Address, user: &Address) {
    if let Some(controller) = pool_access_controller(e, pool) {
        let permissions = AccessControllerClient::new(e, &controller).permissions(pool, user);
        if permissions & BACKSTOP_DEPOSIT_ALLOWED == 0 {
            panic_with_error!(e, BackstopError::UnauthorizedError);
        }
    }
}

/// Require an affirmative controller response proving that backstop deposit
/// permission is absent. Open pools cannot authorize forced exits.
pub fn require_deposit_permission_absent(e: &Env, pool: &Address, user: &Address) {
    let controller = pool_access_controller(e, pool)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::UnauthorizedError));
    let permissions = AccessControllerClient::new(e, &controller).permissions(pool, user);
    if permissions & BACKSTOP_DEPOSIT_ALLOWED != 0 {
        panic_with_error!(e, BackstopError::UnauthorizedError);
    }
}

#[cfg(test)]
mod tests {
    use soroban_sdk::{testutils::Address as _, Address, Env};

    use crate::testutils::{
        create_backstop, create_mock_pool_factory, MockAccessController, MockAccessControllerClient,
    };

    use super::*;

    #[test]
    fn configured_controller_controls_backstop_deposit_permission() {
        let e = Env::default();
        let backstop = create_backstop(&e);
        let pool = Address::generate(&e);
        let user = Address::generate(&e);
        let controller = e.register(MockAccessController, ());
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        factory.set_pool(&pool);
        factory.set_pool_access_controller(&pool, &Some(controller.clone()));
        let controller = MockAccessControllerClient::new(&e, &controller);
        controller.set_permissions(&pool, &user, &BACKSTOP_DEPOSIT_ALLOWED);

        e.as_contract(&backstop, || {
            require_deposit_permission(&e, &pool, &user);
        });
        controller.set_permissions(&pool, &user, &0);
        e.as_contract(&backstop, || {
            require_deposit_permission_absent(&e, &pool, &user);
        });
    }

    #[test]
    #[should_panic]
    fn controller_failure_is_not_absent_permission() {
        let e = Env::default();
        let backstop = create_backstop(&e);
        let pool = Address::generate(&e);
        let user = Address::generate(&e);
        let controller = e.register(MockAccessController, ());
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        factory.set_pool(&pool);
        factory.set_pool_access_controller(&pool, &Some(controller.clone()));
        MockAccessControllerClient::new(&e, &controller).set_fail(&true);

        e.as_contract(&backstop, || {
            require_deposit_permission_absent(&e, &pool, &user);
        });
    }
}
