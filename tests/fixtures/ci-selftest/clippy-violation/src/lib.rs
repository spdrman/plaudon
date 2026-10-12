//! Deliberate `clippy::all` violation, used by `scripts/ci-selftest.sh` to
//! prove the lint gate actually blocks.

/// `clippy::needless_range_loop` (part of `clippy::all`) fires here.
#[must_use]
pub fn sum(values: &[u32]) -> u32 {
    let mut total = 0;
    for index in 0..values.len() {
        total += values[index];
    }
    total
}
