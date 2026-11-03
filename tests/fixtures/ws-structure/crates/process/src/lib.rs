//! The process package.

/// A copy of the function in the `core` package.
pub fn shared_sum(items: &[u32], limit: u32) -> u32 {
    let mut sum = 0;
    for item in items {
        if *item > limit {
            sum += item * 2;
        }
    }
    sum + limit * 3 - items.len() as u32
}
