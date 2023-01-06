use ethers::types::{I256, U256};
use std::mem::swap;

// 2654435769, 1640531526, 4294967296
const INVPHI: i64 = 2654435769; // (math.sqrt(5) - 1) / 2 * 2 ** 32
const INVPHI2: i64 = 1640531526; // (3 - math.sqrt(5)) * 2 ** 32
const DENOM: i64 = 4294967296; // 2 ** 32

pub fn gss<F: Fn(I256) -> I256>(
    f: F,
    mut min_bound: U256,
    mut max_bound: U256,
    tol: I256,
    max_iter: u64,
    honour_bounds: bool,
) -> (U256, U256) {
    let invphi_i256 = I256::from(INVPHI);
    let invphi2_i256 = I256::from(INVPHI2);
    let denom_i256 = I256::from(DENOM);

    if min_bound > max_bound {
        swap(&mut min_bound, &mut max_bound);
    }
    let mut min_bound = I256::from_raw(min_bound);
    let mut max_bound = I256::from_raw(max_bound);

    let mut h = max_bound - min_bound;
    if h <= tol {
        return (I256_to_U256(min_bound), I256_to_U256(max_bound));
    }

    let mut yc = I256::zero();
    let mut xc = I256::zero();

    if honour_bounds {
        xc = min_bound + mul_div(invphi2_i256, h, denom_i256);
        yc = f(xc);
    } else {
        let brackets = bracket(&f, min_bound, max_bound);
        min_bound = brackets.0;
        max_bound = brackets.1;
        xc = brackets.2;
        yc = brackets.3;
    }

    let mut xd = min_bound + mul_div(invphi_i256, h, denom_i256);
    let mut yd = f(xd);

    for _ in 0..max_iter {
        if yc < yd {
            max_bound = xd;
            xd = xc;
            yd = yc;
            h = mul_div(invphi_i256, h, denom_i256);
            xc = min_bound + mul_div(invphi2_i256, h, denom_i256);
            yc = f(xc);
        } else {
            min_bound = xc;
            xc = xd;
            yc = yd;
            h = mul_div(invphi_i256, h, denom_i256);
            xd = min_bound + mul_div(invphi_i256, h, denom_i256);
            yd = f(xd);
        }
    }

    if yc < yd {
        return (I256_to_U256(min_bound), I256_to_U256(xd));
    } else {
        return (I256_to_U256(xc), I256_to_U256(max_bound));
    };
}

fn I256_to_U256(to_convert: I256) -> U256 {
    if to_convert <= I256::zero() {
        return U256::zero();
    }
    return U256::from_dec_str(&to_convert.to_string()).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    // Using the rounding in mul_div this test is unable to find the local minima, because it will keep rounding up to 1.
    // The opposite is true for test_gss_large_interval
    #[test]
    fn test_gss() {
        let func = |x| x * x;
        let min_bound = U256::from(0);
        let max_bound = U256::from(100);
        let tol = I256::from(0);
        let max_iter = 10;
        let honour_bounds = true;

        let res = gss(func, min_bound, max_bound, tol, max_iter, honour_bounds);
        assert_eq!(res.0, U256::from(0))
    }

    // Here we are unable to find one local minima, because the bounds are limited, since we get temporary negative values in the calculation of the provided function
    #[test]
    fn test_gss_multiple_minima() {
        let tol = I256::from(1u128);
        let max_iter = 500;
        let honour_bounds = false;

        let func = |x: I256| {
            ((x - I256::from(2)).pow(6) - (x - I256::from(2)).pow(4) - (x - I256::from(2)).pow(2))
                + I256::from(1)
        };

        let res = gss(
            func,
            U256::from(2u128),
            U256::from(2u128),
            tol,
            max_iter,
            honour_bounds,
        );

        assert_eq!(res.0, U256::from(2));
    }

    // This test uses an input function that can resolve into negative values and therefor limiting the max_bound to 10000.
    // Limiting the max bound and not using the rounnding in mul_div it is unable to find the local minima.
    #[test]
    fn test_gss_large_interval() {
        let f = |x: I256| -> I256 { (I256::from(10000) - x) * (I256::from(10000) - x) };

        let res = gss(
            f,
            U256::from(0),
            U256::from(10000),
            I256::from(1u128),
            10000,
            true,
        );
        assert_eq!(res.0, U256::from(9987));
    }

    #[test]
    fn test_gss_honouring_bounds() {
        let f = |x| x * x;
        let res = gss(
            f,
            U256::from(10u128),
            U256::from(0u128),
            I256::from(1u128),
            100,
            true,
        );
        assert!(res.0 == U256::from(0u128));
    }
}

pub fn mul_div(a: I256, b: I256, denom: I256) -> I256 {
    let product = a * b;

    let result: I256 = (product / denom).try_into().expect("Integer Overflow");

    return result;
}

pub fn bracket<F: Fn(I256) -> I256>(
    f: F,
    mut min_bound: I256,
    mut max_bound: I256,
) -> (I256, I256, I256, I256) {
    let mut min_bound = I256::from_dec_str(&min_bound.to_string()).unwrap();
    let mut max_bound = I256::from_dec_str(&max_bound.to_string()).unwrap();

    let maxiter = I256::from(1000);
    let grow_limit = I256::from(110);
    let GOLDEN_RATIO: I256 = I256::from(6949403065_i64); // golden ratio: (1.0+sqrt(5.0))/2.0 *  2 ** 32
    let denom_i526 = I256::from_dec_str(&DENOM.to_string()).unwrap();
    let _verysmall_num = I256::from(100);
    let _versmall_num_denom = I256::from_dec_str("100000000000000000000000").unwrap();

    let mut ya = f(min_bound);
    let mut yb = f(max_bound);

    if ya < yb {
        swap(&mut min_bound, &mut max_bound);
        swap(&mut ya, &mut yb)
    }
    let mut xc = max_bound + (GOLDEN_RATIO * (max_bound - min_bound)) / denom_i526;
    let mut yc = f(xc);
    let mut yw = I256::zero();
    let mut iter = I256::zero();

    while yc < yb {
        let tmp1 = (max_bound - min_bound) * (yb - yc);
        let tmp2 = (max_bound - xc) * (yb - ya);
        let val = tmp2 - tmp1;
        let mut denom = if val < _verysmall_num {
            (I256::from(2) * _verysmall_num) / _versmall_num_denom
        } else {
            I256::from(2) * val
        };

        let mut w = max_bound - ((max_bound - xc) * tmp2 - (max_bound - min_bound) * tmp1) / denom;
        let wlim = max_bound + grow_limit * (xc - max_bound);

        if iter > maxiter {
            panic!("Too many iterations.");
        }

        iter = iter + I256::one();

        if (w - xc) * (max_bound - w) > I256::zero() {
            yw = f(w);

            if yw < yc {
                let min_bound = max_bound;
                let max_bound = w;
                let xc = xc;
                let yc = yc;
                return (max_bound, min_bound, xc, yc);
            } else if yw > yb {
                let xc = w;
                let yc = yw;
                return (min_bound, max_bound, xc, yc);
            }
            w = xc + (GOLDEN_RATIO * (xc - max_bound)) / denom_i526;
            yw = f(w);
        } else if (w - wlim) * (wlim - xc) >= I256::zero() {
            w = wlim;
            yw = f(w);
        } else if (w - wlim) * (xc - w) > I256::zero() {
            yw = f(w);
            if yw < yc {
                max_bound = xc;
                xc = w;
                w = xc + (GOLDEN_RATIO * (xc - max_bound)) / denom_i526;
                yb = yc;
                yc = yw;
                yw = f(w);
            }
        } else {
            w = xc + (GOLDEN_RATIO * (xc - max_bound)) / denom_i526;
            yw = f(w);
        }
        min_bound = max_bound;
        max_bound = xc;
        xc = w;
        ya = yb;
        yb = yc;
        yc = yw;
    }

    return (min_bound, max_bound, xc, yc);
}

#[cfg(test)]
mod bracket_tests {
    use super::*;

    #[test]
    fn test_bracket() {
        let func = |x: I256| x * x;
        let min_bound = I256::from(0);
        let max_bound = I256::from(10);
        let res = bracket(func, min_bound, max_bound);

        // max_bound
        assert_eq!(res.0, I256::from(10));
        // min_bound
        assert_eq!(res.1, I256::from(0));
        // xc
        assert_eq!(res.2, I256::from(-16));
        // yc
        assert_eq!(res.3, I256::from(256));
    }
}
