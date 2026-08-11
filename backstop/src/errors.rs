use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
/// Error codes for the backstop contract. Common errors are codes that match up with the built-in
/// contracts error reporting. Backstop specific errors start at 1000.
pub enum BackstopError {
    // Common Errors
    InternalError = 1,
    AlreadyInitializedError = 3,

    UnauthorizedError = 4,

    NegativeAmountError = 8,
    BalanceError = 10,
    OverflowError = 12,

    // Backstop
    BadRequest = 1000,
    NotExpired = 1001,
    InvalidRewardZoneEntry = 1002,
    InsufficientFunds = 1003,
    NotPool = 1004,
    InvalidShareMintAmount = 1005,
    InvalidTokenWithdrawAmount = 1006,
    TooManyQ4WEntries = 1007,
    NotInRewardZone = 1008,
    RewardZoneFull = 1009,
    MaxBackfillEmissions = 1010,
    BadDebtExists = 1011,
    AssetConfigurationCollision = 1013,
    InvalidBackstopValuation = 1014,
    InvalidValuation = 1015,
    StaleValuation = 1016,
    InvalidActivationValue = 1017,
    InvalidEmissionValue = 1027,
    DistributionCheckpointRequired = 1028,
    NoEligibleWeight = 1029,
    DistributionTooSoon = 1030,
    InvalidOngoingBalance = 1031,
    EmitterDidNotMigrate = 1032,
    NoOngoingEmissions = 1033,
    PoolEmissionGulpTooSoon = 1034,
    AlreadyFinalized = 1035,
    InvalidQueuedSwap = 1037,
    MigrationNotPrepared = 1039,
    SyncWindowExpired = 1041,
    MigrationNotActive = 1045,
    MigrationEpochNotOpen = 1047,
    BackfillAlreadyFunded = 1049,
    BackfillNotFunded = 1050,
    InvalidBackfillFunding = 1052,
}
