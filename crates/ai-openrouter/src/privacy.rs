//! Destination and privacy-policy resolution (spec 10.5, 10.8).
//!
//! # The finding this module is built on
//!
//! `GET /api/v1/models/:author/:slug/endpoints` (captured as
//! `tests/fixtures/openrouter/model-endpoints-gpt-audio.json`) discloses the
//! downstream **provider name** for each route, and nothing about retention:
//! there is no zero-data-retention field, no policy URL and no data-use flag.
//!
//! Therefore:
//!
//! * the downstream provider is resolvable, so the user can be shown where
//!   their content actually goes;
//! * the retention posture is [`RetentionPosture::Unknown`] unless a
//!   documented policy is supplied for that exact route **and** that exact
//!   content type, and
//! * a user who required documented zero retention is refused, because an
//!   unresolved posture never satisfies a requirement.
//!
//! A text-generation policy never answers for a speech-to-text upload: the
//! resolution is keyed by [`ContentType`] as well as by route (spec 10.8).

use std::collections::BTreeMap;

use plaudon_security::consent::{ContentType, ProviderRoute, RetentionPosture, RELAY_OPENROUTER};
use serde::{Deserialize, Serialize};

/// One route as reported by the endpoints metadata.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EndpointRecord {
    /// Route name as reported, e.g. `OpenAI | openai/gpt-audio`.
    #[serde(default)]
    pub name: String,
    /// Downstream provider, when reported.
    #[serde(default)]
    pub provider_name: Option<String>,
    /// Context length for this route.
    #[serde(default)]
    pub context_length: Option<u64>,
    /// Per-unit prices for this route.
    #[serde(default)]
    pub pricing: BTreeMap<String, serde_json::Value>,
    /// Anything else reported, preserved rather than reinterpreted.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

/// The `data` block of an endpoints response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EndpointsData {
    /// Model identifier.
    #[serde(default)]
    pub id: String,
    /// Routes serving the model.
    #[serde(default)]
    pub endpoints: Vec<EndpointRecord>,
}

/// A decoded endpoints response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EndpointsResponse {
    /// Payload.
    #[serde(default)]
    pub data: EndpointsData,
}

impl EndpointsResponse {
    /// The first route that names a downstream provider, which is the one a
    /// request without routing preferences would reach.
    #[must_use]
    pub fn primary_provider(&self) -> Option<&str> {
        self.data
            .endpoints
            .iter()
            .find_map(|endpoint| endpoint.provider_name.as_deref())
    }

    /// Whether any route discloses a retention or data-use control.
    ///
    /// Always `false` against the captured contract; kept as a function so a
    /// future endpoint revision that *does* disclose one is picked up by the
    /// fixture test rather than by an assumption in prose.
    #[must_use]
    pub fn discloses_retention(&self) -> bool {
        const RETENTION_KEYS: [&str; 5] = [
            "data_policy",
            "retention",
            "zero_data_retention",
            "data_retention",
            "privacy_policy_url",
        ];
        self.data.endpoints.iter().any(|endpoint| {
            RETENTION_KEYS
                .iter()
                .any(|key| endpoint.unknown.contains_key(*key))
        })
    }
}

/// Where a retention posture came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicySource {
    /// The provider's endpoint metadata stated it for this exact route.
    EndpointMetadata,
    /// The user configured a documented policy for this exact route and
    /// content type in settings.
    UserConfiguredPolicy,
    /// Nothing resolved it.
    Unresolved,
}

/// The resolved destination for one dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacyResolution {
    /// Relay, model and downstream provider.
    pub route: ProviderRoute,
    /// What would be sent. A resolution for one content type says nothing
    /// about another.
    pub content_type: ContentType,
    /// Posture resolved for this exact route and content type.
    pub posture: RetentionPosture,
    /// Where the posture came from.
    pub source: PolicySource,
}

impl PrivacyResolution {
    /// The snapshot string recorded on the inference-attempt row (spec 10.7).
    ///
    /// Stable, non-secret and comparable: a change in any component is a change
    /// of destination, which invalidates the consent grant (spec 10.8).
    #[must_use]
    pub fn snapshot(&self) -> String {
        format!(
            "relay={};model={};provider={};content={};retention={};source={}",
            self.route.relay,
            self.route.model_id,
            self.route
                .downstream_provider
                .as_deref()
                .unwrap_or("<undisclosed>"),
            self.content_type.label(),
            self.posture,
            match self.source {
                PolicySource::EndpointMetadata => "endpoint_metadata",
                PolicySource::UserConfiguredPolicy => "user_configured",
                PolicySource::Unresolved => "unresolved",
            }
        )
    }

    /// Whether the posture resolved at all.
    #[must_use]
    pub const fn is_resolved(&self) -> bool {
        self.posture.is_resolved()
    }
}

/// A documented policy the user configured for an exact route and content type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentedPolicy {
    /// Model the policy applies to.
    pub model_id: String,
    /// Downstream provider the policy applies to. `None` never matches a named
    /// provider, in either direction.
    pub downstream_provider: Option<String>,
    /// Content type the policy covers. A transcript policy does not cover an
    /// audio upload.
    pub content_type: ContentType,
    /// The documented posture.
    pub posture: RetentionPosture,
}

/// Resolve the destination and its retention posture.
///
/// `documented` holds policies the user recorded for exact routes; the
/// provider's own metadata is consulted first and currently never carries one.
#[must_use]
pub fn resolve(
    model_id: &str,
    content_type: ContentType,
    endpoints: Option<&EndpointsResponse>,
    documented: &[DocumentedPolicy],
) -> PrivacyResolution {
    let downstream = endpoints
        .and_then(EndpointsResponse::primary_provider)
        .map(str::to_owned);
    let mut route = ProviderRoute::new(RELAY_OPENROUTER, model_id);
    route.downstream_provider = downstream;

    let matching = documented.iter().find(|policy| {
        policy.model_id == model_id
            && policy.content_type == content_type
            && policy.downstream_provider == route.downstream_provider
    });

    match matching {
        Some(policy) => PrivacyResolution {
            route,
            content_type,
            posture: policy.posture,
            source: PolicySource::UserConfiguredPolicy,
        },
        None => PrivacyResolution {
            route,
            content_type,
            // The provider metadata carries no retention field; saying
            // "provider default" here would be a claim we cannot support.
            posture: RetentionPosture::Unknown,
            source: PolicySource::Unresolved,
        },
    }
}

/// Why a dispatch was refused on privacy grounds.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrivacyRefusal {
    /// The user required a restriction and the posture did not resolve.
    #[error("the retention policy for this exact model and route could not be confirmed")]
    Unresolved {
        /// Non-secret route label.
        route_label: String,
    },
    /// The posture resolved but is weaker than required.
    #[error("the retention policy for this route is weaker than required")]
    Weaker {
        /// Non-secret route label.
        route_label: String,
        /// What the user required.
        required: RetentionPosture,
        /// What the route actually offers.
        resolved: RetentionPosture,
    },
}

/// Enforce a privacy requirement, failing closed (spec 10.5).
///
/// # Errors
///
/// Returns [`PrivacyRefusal`] when a required posture cannot be confirmed for
/// this exact route and content type, or is weaker than required.
pub fn enforce(
    resolution: &PrivacyResolution,
    required: Option<RetentionPosture>,
) -> Result<(), PrivacyRefusal> {
    let Some(required) = required else {
        return Ok(());
    };
    if !resolution.posture.is_resolved() {
        return Err(PrivacyRefusal::Unresolved {
            route_label: resolution.route.label(),
        });
    }
    if required.satisfied_by(resolution.posture) {
        Ok(())
    } else {
        Err(PrivacyRefusal::Weaker {
            route_label: resolution.route.label(),
            required,
            resolved: resolution.posture,
        })
    }
}

/// The non-retroactivity disclosure shown wherever zero retention is offered
/// (spec 10.5). Fixed text so the UI cannot soften it.
pub const ZERO_RETENTION_CAVEAT: &str = "Zero retention still means the content leaves this device and is processed by the provider. It does not delete data the provider, this app's exports, logs or your backups already hold.";

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoints_with_provider(provider: &str) -> EndpointsResponse {
        EndpointsResponse {
            data: EndpointsData {
                id: "openai/gpt-audio".to_owned(),
                endpoints: vec![EndpointRecord {
                    name: "OpenAI | openai/gpt-audio".to_owned(),
                    provider_name: Some(provider.to_owned()),
                    ..EndpointRecord::default()
                }],
            },
        }
    }

    #[test]
    fn the_downstream_provider_is_disclosed_when_the_metadata_names_it() {
        let resolution = resolve(
            "openai/gpt-audio",
            ContentType::AudioBytes,
            Some(&endpoints_with_provider("OpenAI")),
            &[],
        );
        assert_eq!(
            resolution.route.downstream_provider.as_deref(),
            Some("OpenAI")
        );
        assert!(resolution.route.has_known_downstream_provider());
    }

    #[test]
    fn an_absent_retention_field_resolves_to_unknown_not_provider_default() {
        let resolution = resolve(
            "openai/gpt-audio",
            ContentType::AudioBytes,
            Some(&endpoints_with_provider("OpenAI")),
            &[],
        );
        assert_eq!(resolution.posture, RetentionPosture::Unknown);
        assert_eq!(resolution.source, PolicySource::Unresolved);
        assert!(!resolution.is_resolved());
    }

    #[test]
    fn a_required_restriction_fails_closed_when_nothing_resolved() {
        let resolution = resolve("openai/gpt-audio", ContentType::AudioBytes, None, &[]);
        let refusal = enforce(&resolution, Some(RetentionPosture::ZeroRetention))
            .expect_err("must fail closed");
        assert!(matches!(refusal, PrivacyRefusal::Unresolved { .. }));
        // With no requirement, the same unresolved route is allowed to proceed
        // behind an explicit consent grant instead.
        assert!(enforce(&resolution, None).is_ok());
    }

    #[test]
    fn a_transcript_policy_does_not_answer_for_an_audio_upload() {
        let documented = vec![DocumentedPolicy {
            model_id: "openai/gpt-audio".to_owned(),
            downstream_provider: Some("OpenAI".to_owned()),
            content_type: ContentType::TranscriptText,
            posture: RetentionPosture::ZeroRetention,
        }];
        let endpoints = endpoints_with_provider("OpenAI");

        let audio = resolve(
            "openai/gpt-audio",
            ContentType::AudioBytes,
            Some(&endpoints),
            &documented,
        );
        assert_eq!(audio.posture, RetentionPosture::Unknown);
        assert!(enforce(&audio, Some(RetentionPosture::ZeroRetention)).is_err());

        let text = resolve(
            "openai/gpt-audio",
            ContentType::TranscriptText,
            Some(&endpoints),
            &documented,
        );
        assert_eq!(text.posture, RetentionPosture::ZeroRetention);
        assert!(enforce(&text, Some(RetentionPosture::ZeroRetention)).is_ok());
    }

    #[test]
    fn a_policy_for_an_undisclosed_provider_does_not_cover_a_named_one() {
        let documented = vec![DocumentedPolicy {
            model_id: "openai/gpt-audio".to_owned(),
            downstream_provider: None,
            content_type: ContentType::AudioBytes,
            posture: RetentionPosture::ZeroRetention,
        }];
        let resolution = resolve(
            "openai/gpt-audio",
            ContentType::AudioBytes,
            Some(&endpoints_with_provider("OpenAI")),
            &documented,
        );
        assert_eq!(resolution.posture, RetentionPosture::Unknown);
    }

    #[test]
    fn a_weaker_resolved_posture_is_refused_with_both_sides_named() {
        let documented = vec![DocumentedPolicy {
            model_id: "m".to_owned(),
            downstream_provider: None,
            content_type: ContentType::AudioBytes,
            posture: RetentionPosture::LimitedRetention { days: 30 },
        }];
        let resolution = resolve("m", ContentType::AudioBytes, None, &documented);
        let refusal =
            enforce(&resolution, Some(RetentionPosture::ZeroRetention)).expect_err("refused");
        assert_eq!(
            refusal,
            PrivacyRefusal::Weaker {
                route_label: resolution.route.label(),
                required: RetentionPosture::ZeroRetention,
                resolved: RetentionPosture::LimitedRetention { days: 30 },
            }
        );
    }

    #[test]
    fn the_snapshot_changes_when_any_component_changes() {
        let base = resolve("m", ContentType::AudioBytes, None, &[]);
        let other_model = resolve("n", ContentType::AudioBytes, None, &[]);
        let other_content = resolve("m", ContentType::TranscriptText, None, &[]);
        assert_ne!(base.snapshot(), other_model.snapshot());
        assert_ne!(base.snapshot(), other_content.snapshot());
        assert!(!base.snapshot().contains("secret"));
    }
}
