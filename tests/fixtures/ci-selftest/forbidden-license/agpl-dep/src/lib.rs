//! A dependency whose declared licence the policy forbids.
#[must_use]
pub fn marker() -> &'static str {
    "forbidden-licence fixture"
}
