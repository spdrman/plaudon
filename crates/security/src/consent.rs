//! Scoped consent grants and the cloud dispatch preflight (§16.3, §4.4, §10.8).
//!
//! # Consent is a scoped capability, not a global checkbox
//!
//! A [`ConsentGrant`] is narrow on five independent axes, and a dispatch is
//! covered only when **every** axis matches:
//!
//! | Axis | Type | Widening requires |
//! |---|---|---|
//! | why | [`ConsentPurpose`] | a new prompt |
//! | what | [`ContentType`] | a new prompt |
//! | where | [`ProviderRoute`] (relay + model + downstream provider) | a new prompt |
//! | how long it is kept | [`RetentionPosture`] | a new prompt |
//! | how broadly | [`GrantScope`] — the three §4.4 choices | a new prompt |
//!
//! Each grant also pins the `disclosure_version` the user actually read. When
//! the privacy statement changes, existing grants stop satisfying the preflight
//! and the user is asked again (§4.4(5)).
//!
//! # The presence of an API key is never consent
//!
//! §16.3 is explicit: *"We will not infer consent from the mere presence of an
//! API key."* Nothing in this module reads the credential store, and
//! [`dispatch_preflight`] takes no secret, no key handle and no "key is
//! configured" flag. A user who saved an `OpenRouter` key and has no matching
//! grant gets `CONSENT_REQUIRED`, every time. Saving a key is a setup step;
//! sending a specific recording to a specific model is a separate decision.
//!
//! # Composition with Local-only Mode
//!
//! [`dispatch_preflight`] runs [`crate::outbound::OutboundPolicy::authorize`]
//! **first and unconditionally**. No quantity or breadth of consent can
//! re-enable egress while Local-only Mode is on (§16.2A): a perfectly matching
//! grant under `OutboundPolicy::local_only()` still yields `EGRESS_DENIED`.
//!
//! The two gates are also joined in the type system. A successful preflight
//! returns a [`DispatchApproval`] that *carries* the [`crate::outbound::EgressTicket`],
//! and a ticket cannot be constructed outside [`crate::outbound`]. A dispatcher
//! that demands a ticket therefore cannot send without having passed both the
//! egress policy and the consent preflight.
//!
//! # Tri-state discipline
//!
//! [`RetentionPosture::Unknown`] is a real state, not a default. It satisfies no
//! requirement and is satisfied by no requirement. Likewise a
//! [`ProviderRoute`] whose `downstream_provider` is [`None`] means "we could not
//! discover who runs this model", and it never matches a route where the user
//! was shown a named provider (§10.8, §2.5B).
//!
//! # What this module does not do
//!
//! It does not transfer anything, does not know whether a provider honours its
//! published policy, and does not retroactively delete data a provider already
//! holds. A grant records what the user approved and what we displayed when
//! they approved it; it is not evidence of provider behaviour (§10.5, §16.6).

mod preflight;
mod scope;
mod store;

pub use preflight::{
    dispatch_preflight, ConsentError, ConsentPrompt, DispatchApproval, DispatchRequest, ScopeAxis,
};
pub use scope::{
    relay_endpoint, ConsentGrant, ConsentPurpose, ContentType, GrantScope,
    PayloadClassNotConsentable, ProviderRoute, RequestScope, RetentionPosture, RELAY_OPENROUTER,
};
pub use store::{ConsentLedger, ConsentRepository};

#[cfg(test)]
mod tests;
