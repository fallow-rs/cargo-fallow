//! The core.

/// A function over both complexity limits.
pub fn branchy(values: &[i32], strict: bool) -> i32 {
    let mut total = 0;
    for value in values {
        if *value > 0 && strict {
            total += 1;
        } else if *value < 0 || !strict {
            total -= 1;
        } else {
            match value {
                0 => total += 2,
                _ => total += 3,
            }
        }
    }
    total
}

// fallow-ignore-next-line complexity
pub fn quiet(values: &[i32], strict: bool) -> i32 {
    let mut total = 0;
    for value in values {
        if *value > 0 && strict {
            total += 1;
        } else if *value < 0 || !strict {
            total -= 1;
        } else {
            match value {
                0 => total += 2,
                _ => total += 3,
            }
        }
    }
    total
}

/// The copy of this function is in the `process` package.
pub fn shared_sum(items: &[u32], limit: u32) -> u32 {
    let mut sum = 0;
    for item in items {
        if *item > limit {
            sum += item * 2;
        }
    }
    sum + limit * 3 - items.len() as u32
}
