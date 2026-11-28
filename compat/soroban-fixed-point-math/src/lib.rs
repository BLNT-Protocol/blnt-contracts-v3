#![no_std]

use soroban_sdk::{unwrap::UnwrapOptimized, Env, I256};

/// Fixed-point arithmetic used by the Blend v2 implementation.
pub trait FixedPoint: Sized {
    fn fixed_mul_floor(self, y: Self, denominator: Self) -> Option<Self>;
    fn fixed_mul_ceil(self, y: Self, denominator: Self) -> Option<Self>;
    fn fixed_div_floor(self, y: Self, denominator: Self) -> Option<Self>;
    fn fixed_div_ceil(self, y: Self, denominator: Self) -> Option<Self>;
}

/// Fixed-point arithmetic that promotes overflowing i128 intermediates to I256.
pub trait SorobanFixedPoint: Sized {
    fn fixed_mul_floor(&self, env: &Env, y: &Self, denominator: &Self) -> Self;
    fn fixed_mul_ceil(&self, env: &Env, y: &Self, denominator: &Self) -> Self;
    fn fixed_div_floor(&self, env: &Env, y: &Self, denominator: &Self) -> Self;
    fn fixed_div_ceil(&self, env: &Env, y: &Self, denominator: &Self) -> Self;
}

impl FixedPoint for i128 {
    fn fixed_mul_floor(self, y: i128, denominator: i128) -> Option<i128> {
        mul_div_floor_i128(self, y, denominator)
    }

    fn fixed_mul_ceil(self, y: i128, denominator: i128) -> Option<i128> {
        mul_div_ceil_i128(self, y, denominator)
    }

    fn fixed_div_floor(self, y: i128, denominator: i128) -> Option<i128> {
        mul_div_floor_i128(self, denominator, y)
    }

    fn fixed_div_ceil(self, y: i128, denominator: i128) -> Option<i128> {
        mul_div_ceil_i128(self, denominator, y)
    }
}

fn mul_div_floor_i128(x: i128, y: i128, z: i128) -> Option<i128> {
    div_floor_i128(x.checked_mul(y)?, z)
}

fn div_floor_i128(value: i128, denominator: i128) -> Option<i128> {
    if value < 0 || (value > 0 && denominator < 0) {
        let remainder = value.checked_rem_euclid(denominator)?;
        (value / denominator).checked_sub(if remainder > 0 { 1 } else { 0 })
    } else {
        value.checked_div(denominator)
    }
}

fn mul_div_ceil_i128(x: i128, y: i128, z: i128) -> Option<i128> {
    div_ceil_i128(x.checked_mul(y)?, z)
}

fn div_ceil_i128(value: i128, denominator: i128) -> Option<i128> {
    if value <= 0 || (value > 0 && denominator < 0) {
        value.checked_div(denominator)
    } else {
        let remainder = value.checked_rem_euclid(denominator)?;
        (value / denominator).checked_add(if remainder > 0 { 1 } else { 0 })
    }
}

impl SorobanFixedPoint for i128 {
    fn fixed_mul_floor(&self, env: &Env, y: &i128, denominator: &i128) -> i128 {
        scaled_mul_div_floor(self, env, y, denominator)
    }

    fn fixed_mul_ceil(&self, env: &Env, y: &i128, denominator: &i128) -> i128 {
        scaled_mul_div_ceil(self, env, y, denominator)
    }

    fn fixed_div_floor(&self, env: &Env, y: &i128, denominator: &i128) -> i128 {
        scaled_mul_div_floor(self, env, denominator, y)
    }

    fn fixed_div_ceil(&self, env: &Env, y: &i128, denominator: &i128) -> i128 {
        scaled_mul_div_ceil(self, env, denominator, y)
    }
}

fn scaled_mul_div_floor(x: &i128, env: &Env, y: &i128, z: &i128) -> i128 {
    match x.checked_mul(*y) {
        Some(value) => div_floor_i128(value, *z).unwrap_optimized(),
        None => mul_div_floor_i256(
            env,
            &I256::from_i128(env, *x),
            &I256::from_i128(env, *y),
            &I256::from_i128(env, *z),
        )
        .to_i128()
        .unwrap_optimized(),
    }
}

fn scaled_mul_div_ceil(x: &i128, env: &Env, y: &i128, z: &i128) -> i128 {
    match x.checked_mul(*y) {
        Some(value) => div_ceil_i128(value, *z).unwrap_optimized(),
        None => mul_div_ceil_i256(
            env,
            &I256::from_i128(env, *x),
            &I256::from_i128(env, *y),
            &I256::from_i128(env, *z),
        )
        .to_i128()
        .unwrap_optimized(),
    }
}

impl SorobanFixedPoint for I256 {
    fn fixed_mul_floor(&self, env: &Env, y: &I256, denominator: &I256) -> I256 {
        mul_div_floor_i256(env, self, y, denominator)
    }

    fn fixed_mul_ceil(&self, env: &Env, y: &I256, denominator: &I256) -> I256 {
        mul_div_ceil_i256(env, self, y, denominator)
    }

    fn fixed_div_floor(&self, env: &Env, y: &I256, denominator: &I256) -> I256 {
        mul_div_floor_i256(env, self, denominator, y)
    }

    fn fixed_div_ceil(&self, env: &Env, y: &I256, denominator: &I256) -> I256 {
        mul_div_ceil_i256(env, self, denominator, y)
    }
}

fn mul_div_floor_i256(env: &Env, x: &I256, y: &I256, z: &I256) -> I256 {
    let zero = I256::from_i32(env, 0);
    let value = x.mul(y);
    if value < zero || (value > zero && z.clone() < zero) {
        let remainder = value.rem_euclid(z);
        let one = I256::from_i32(env, 1);
        value
            .div(z)
            .sub(if remainder > zero { &one } else { &zero })
    } else {
        value.div(z)
    }
}

fn mul_div_ceil_i256(env: &Env, x: &I256, y: &I256, z: &I256) -> I256 {
    let zero = I256::from_i32(env, 0);
    let value = x.mul(y);
    if value <= zero || (value > zero && z.clone() < zero) {
        value.div(z)
    } else {
        let remainder = value.rem_euclid(z);
        let one = I256::from_i32(env, 1);
        value
            .div(z)
            .add(if remainder > zero { &one } else { &zero })
    }
}
