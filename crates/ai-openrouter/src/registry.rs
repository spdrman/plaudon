//! The capability registry: what the picker is allowed to say about a model
//! (spec 10.1, 10.2, 8.6A, App. A.3).
//!
//! Three inputs, with strictly different authority:
//!
//! | Input | May set | May never set |
//! |---|---|---|
//! | pinned allowlist | candidate family, fixture id | verification |
//! | live catalog | display name, context, pricing, "no audio modality at all" | request family, features, verification |
//! | observed exchange | everything, including verification | — |
//!
//! [`ModelRegistry::apply_probe`] is the only function that can raise a card to
//! [`Verification::Verified`], and only from a [`ProbeEvidence::ObservedExchange`]
//! record that also carries a resolved route. Everything else is monotone
//! downward: a merge can turn a model into "unsupported", never into "verified".

use std::collections::BTreeMap;

use plaudon_ai_contracts::capabilities::RequestFamily;
use plaudon_domain::capability::{Known, Tri};
use plaudon_domain::locale::LanguageTag;
use serde::{Deserialize, Serialize};

use crate::allowlist::{Allowlist, AllowlistEntry, ModelTask};
use crate::catalog::{CatalogEntry, ModelProbeRecord, ProbeEvidence, TimestampSupport};
use crate::pricing::PricingUnits;
use crate::privacy::PrivacyResolution;

/// Engine identifier for every OpenRouter-routed model.
pub const OPENROUTER_ENGINE_ID: &str = "openrouter";

/// Why a model is not verified. A closed vocabulary so the UI never invents an
/// explanation (spec 15.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotVerifiedReason {
    /// Neither pinned nor probed.
    NotInAllowlist,
    /// Pinned or discovered, but no exchange has been observed.
    NoObservedExchange,
    /// Only the public catalog has been read.
    CatalogOnly,
    /// An exchange was observed but the privacy route did not resolve.
    PrivacyRouteUnresolved,
    /// The observed family has no request builder.
    NoRequestBuilder,
    /// The user typed the identifier by hand (spec 10.2 advanced field).
    UserSuppliedCustomId,
}

impl NotVerifiedReason {
    /// Stable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            NotVerifiedReason::NotInAllowlist => "not_in_allowlist",
            NotVerifiedReason::NoObservedExchange => "no_observed_exchange",
            NotVerifiedReason::CatalogOnly => "catalog_only",
            NotVerifiedReason::PrivacyRouteUnresolved => "privacy_route_unresolved",
            NotVerifiedReason::NoRequestBuilder => "no_request_builder",
            NotVerifiedReason::UserSuppliedCustomId => "user_supplied_custom_id",
        }
    }
}

/// Verification state of one model card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Verification {
    /// A capability smoke test passed against this API contract.
    Verified {
        /// Fixture or probe identifier the verification is recorded against.
        fixture_id: String,
        /// Date of the observation, `YYYY-MM-DD`.
        validated_at: String,
    },
    /// Everything else.
    NotVerified {
        /// Why.
        reason: NotVerifiedReason,
    },
}

impl Verification {
    /// Whether this card may be dispatched to.
    #[must_use]
    pub const fn is_verified(&self) -> bool {
        matches!(self, Verification::Verified { .. })
    }
}

/// Observed feature matrix. Every field is a tri-state: unknown is not support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureMatrix {
    /// Segment-level offsets observed in a response.
    pub segments: Tri,
    /// Word-level offsets observed in a response.
    pub word_timestamps: Tri,
    /// Speaker turns observed in a response.
    pub diarization: Tri,
    /// A detected language observed in a response.
    pub language_detection: Tri,
}

impl Default for FeatureMatrix {
    fn default() -> Self {
        Self {
            segments: Tri::Unknown,
            word_timestamps: Tri::Unknown,
            diarization: Tri::Unknown,
            language_detection: Tri::Unknown,
        }
    }
}

/// Everything the picker may display about one model (spec 10.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCard {
    /// Provider-scoped identifier.
    pub model_id: String,
    /// Name to show.
    pub display_name: String,
    /// Stage this card is for. The same model may be a candidate for both.
    pub task: ModelTask,
    /// Validated request family. Only an observed exchange can set this to a
    /// family with a builder.
    pub request_family: RequestFamily,
    /// Verification state.
    pub verification: Verification,
    /// Observed features.
    pub features: FeatureMatrix,
    /// Locales the model declared, when any were observed.
    pub locales: Known<Vec<LanguageTag>>,
    /// Largest payload an observed exchange accepted.
    pub max_input_bytes: Known<u64>,
    /// Longest media an observed exchange accepted.
    pub max_duration_ms: Known<u64>,
    /// Context length the catalog reported.
    pub context_length: Known<u64>,
    /// Per-unit prices.
    pub pricing: PricingUnits,
    /// Resolved destination and retention posture, when resolution ran.
    pub privacy: Option<PrivacyResolution>,
    /// Whether the catalog says the model accepts audio at all.
    pub declares_audio_input: Tri,
}

impl ModelCard {
    /// A card built from a pinned entry alone: pinned, never verified.
    #[must_use]
    pub fn from_allowlist(entry: &AllowlistEntry) -> Self {
        Self {
            model_id: entry.model_id.clone(),
            display_name: entry.display_name.clone(),
            task: entry.task,
            // A pinned entry records which family our fixtures cover; it is a
            // candidate, so the card still reports `Unknown` until observed.
            request_family: RequestFamily::Unknown,
            verification: Verification::NotVerified {
                reason: NotVerifiedReason::NoObservedExchange,
            },
            features: FeatureMatrix::default(),
            locales: Known::Unknown,
            max_input_bytes: Known::Unknown,
            max_duration_ms: Known::Unknown,
            context_length: Known::Unknown,
            pricing: PricingUnits::unknown(),
            privacy: None,
            declares_audio_input: Tri::Unknown,
        }
    }

    /// A card for a model id the user typed by hand (spec 10.2).
    #[must_use]
    pub fn custom(model_id: impl Into<String>, task: ModelTask) -> Self {
        let model_id = model_id.into();
        Self {
            display_name: model_id.clone(),
            model_id,
            task,
            request_family: RequestFamily::Unknown,
            verification: Verification::NotVerified {
                reason: NotVerifiedReason::UserSuppliedCustomId,
            },
            features: FeatureMatrix::default(),
            locales: Known::Unknown,
            max_input_bytes: Known::Unknown,
            max_duration_ms: Known::Unknown,
            context_length: Known::Unknown,
            pricing: PricingUnits::unknown(),
            privacy: None,
            declares_audio_input: Tri::Unknown,
        }
    }

    /// Whether this card may be dispatched to right now.
    ///
    /// Transcription additionally requires a family with a builder: a verified
    /// text model is not a transcription engine (spec 8.4 "we will not use a
    /// text-only chat model as if it can decode audio").
    #[must_use]
    pub fn is_selectable(&self) -> bool {
        if !self.verification.is_verified() {
            return false;
        }
        match self.task {
            ModelTask::Transcription => self.request_family.has_builder(),
            ModelTask::TextGeneration => true,
        }
    }
}

/// The merged view of every model this build knows about.
#[derive(Debug, Clone, Default)]
pub struct ModelRegistry {
    cards: BTreeMap<(ModelTask, String), ModelCard>,
    catalog_fetched_at: Option<String>,
    source: CatalogSource,
}

/// Where the current card set came from (spec 10.1 degraded discovery).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSource {
    /// Only the pinned allowlist has been applied.
    #[default]
    PinnedAllowlist,
    /// A live catalog response has been merged on top of it.
    Live,
}

impl ModelRegistry {
    /// Seed the registry from the pinned allowlist.
    #[must_use]
    pub fn from_allowlist(allowlist: &Allowlist) -> Self {
        let mut cards = BTreeMap::new();
        for entry in allowlist.entries() {
            cards.insert(
                (entry.task, entry.model_id.clone()),
                ModelCard::from_allowlist(entry),
            );
        }
        Self {
            cards,
            catalog_fetched_at: None,
            source: CatalogSource::PinnedAllowlist,
        }
    }

    /// Where the current cards came from.
    #[must_use]
    pub const fn source(&self) -> CatalogSource {
        self.source
    }

    /// When the live catalog was fetched, when it was.
    #[must_use]
    pub fn catalog_fetched_at(&self) -> Option<&str> {
        self.catalog_fetched_at.as_deref()
    }

    /// Merge a live catalog response.
    ///
    /// Adds display metadata and prices for models already known, and adds
    /// audio-capable models not on the pinned list as transcription
    /// *candidates*. It never sets a request family, a feature or a
    /// verification: the catalog cannot establish any of them (spec 8.6).
    pub fn apply_catalog(&mut self, entries: &[CatalogEntry], fetched_at: impl Into<String>) {
        for entry in entries {
            let declares_audio = Tri::from_observation(Some(entry.declares_audio_input()));
            for task in [ModelTask::Transcription, ModelTask::TextGeneration] {
                if task == ModelTask::Transcription && !entry.declares_audio_input() {
                    // The one downgrade the catalog is allowed to express.
                    if let Some(card) = self.cards.get_mut(&(task, entry.id.clone())) {
                        card.request_family = RequestFamily::Unsupported;
                        card.declares_audio_input = declares_audio;
                    }
                    continue;
                }
                let card = self
                    .cards
                    .entry((task, entry.id.clone()))
                    .or_insert_with(|| ModelCard {
                        verification: Verification::NotVerified {
                            reason: NotVerifiedReason::CatalogOnly,
                        },
                        ..ModelCard::from_allowlist(&AllowlistEntry {
                            model_id: entry.id.clone(),
                            display_name: entry.name.clone(),
                            task,
                            candidate_family: RequestFamily::Unknown,
                            fixture_id: String::new(),
                            notes: String::new(),
                        })
                    });
                if !entry.name.is_empty() {
                    card.display_name = entry.name.clone();
                }
                card.declares_audio_input = declares_audio;
                card.context_length = match entry.context_length {
                    Some(length) => Known::Known(length),
                    None => Known::Unknown,
                };
                card.pricing = PricingUnits::from_catalog(entry);
            }
        }
        self.catalog_fetched_at = Some(fetched_at.into());
        self.source = CatalogSource::Live;
    }

    /// Apply an observed probe record.
    ///
    /// The only path to [`Verification::Verified`], and it still refuses when
    /// the record itself is not evidence: a catalog-only record, a family with
    /// no builder, or an unresolved route all leave the card unverified with
    /// the matching reason (spec 10.1, 10.8).
    pub fn apply_probe(&mut self, task: ModelTask, record: &ModelProbeRecord, fixture_id: &str) {
        let card = self
            .cards
            .entry((task, record.model_id.clone()))
            .or_insert_with(|| ModelCard::custom(record.model_id.clone(), task));

        card.features = FeatureMatrix {
            segments: Tri::from_observation(Some(matches!(
                record.timestamps,
                TimestampSupport::ObservedSegments | TimestampSupport::ObservedWords
            ))),
            word_timestamps: Tri::from_observation(Some(matches!(
                record.timestamps,
                TimestampSupport::ObservedWords
            ))),
            diarization: record.diarization,
            language_detection: record.language_detection,
        };
        card.max_input_bytes = record.max_input_bytes.clone();
        card.max_duration_ms = record.max_duration_ms.clone();

        if record.evidence != ProbeEvidence::ObservedExchange {
            card.verification = Verification::NotVerified {
                reason: NotVerifiedReason::CatalogOnly,
            };
            return;
        }
        card.request_family = record.request_family;
        if task == ModelTask::Transcription && !record.request_family.has_builder() {
            card.verification = Verification::NotVerified {
                reason: NotVerifiedReason::NoRequestBuilder,
            };
            return;
        }
        if !record.provider_route.is_known() {
            card.verification = Verification::NotVerified {
                reason: NotVerifiedReason::PrivacyRouteUnresolved,
            };
            return;
        }
        card.verification = Verification::Verified {
            fixture_id: fixture_id.to_owned(),
            validated_at: record.observed_on.clone(),
        };
    }

    /// Attach a resolved destination to a card.
    pub fn apply_privacy(&mut self, task: ModelTask, resolution: PrivacyResolution) {
        if let Some(card) = self
            .cards
            .get_mut(&(task, resolution.route.model_id.clone()))
        {
            card.privacy = Some(resolution);
        }
    }

    /// Add a user-typed model id as an unverified candidate.
    pub fn add_custom(&mut self, model_id: &str, task: ModelTask) -> &ModelCard {
        self.cards
            .entry((task, model_id.to_owned()))
            .or_insert_with(|| ModelCard::custom(model_id, task))
    }

    /// One card.
    #[must_use]
    pub fn card(&self, model_id: &str, task: ModelTask) -> Option<&ModelCard> {
        self.cards.get(&(task, model_id.to_owned()))
    }

    /// Every card for one stage, in identifier order.
    #[must_use]
    pub fn for_task(&self, task: ModelTask) -> Vec<&ModelCard> {
        self.cards
            .iter()
            .filter(|((card_task, _), _)| *card_task == task)
            .map(|(_, card)| card)
            .collect()
    }

    /// Number of cards.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cards.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plaudon_security::consent::{ContentType, RetentionPosture};

    fn registry() -> ModelRegistry {
        ModelRegistry::from_allowlist(&Allowlist::embedded().expect("manifest"))
    }

    fn observed(model_id: &str) -> ModelProbeRecord {
        ModelProbeRecord {
            model_id: model_id.to_owned(),
            request_family: RequestFamily::MultipartStt,
            evidence: ProbeEvidence::ObservedExchange,
            accepted_formats: Known::Known(vec!["wav".to_owned()]),
            max_input_bytes: Known::Known(26_214_400),
            max_duration_ms: Known::Known(1_500_000),
            timestamps: TimestampSupport::ObservedSegments,
            diarization: Tri::No,
            language_detection: Tri::Yes,
            provider_route: Known::Known("OpenAI".to_owned()),
            retention_control: Known::Unknown,
            audio_input_price: Known::Unknown,
            pricing_unit: Known::Unknown,
            observed_on: "2026-10-11".to_owned(),
            notes: String::new(),
        }
    }

    #[test]
    fn pinned_models_start_unverified() {
        let registry = registry();
        assert!(!registry.is_empty());
        for card in registry.for_task(ModelTask::Transcription) {
            assert_eq!(
                card.verification,
                Verification::NotVerified {
                    reason: NotVerifiedReason::NoObservedExchange
                },
                "{} must not ship pre-verified",
                card.model_id
            );
            assert!(!card.is_selectable());
        }
    }

    #[test]
    fn the_catalog_alone_never_verifies_or_sets_a_family() {
        let mut registry = registry();
        let entry = CatalogEntry {
            id: "openai/gpt-audio".to_owned(),
            name: "OpenAI: GPT Audio".to_owned(),
            context_length: Some(128_000),
            architecture: crate::catalog::Architecture {
                input_modalities: vec!["text".to_owned(), "audio".to_owned()],
                output_modalities: vec!["text".to_owned()],
            },
            pricing: BTreeMap::from([("audio".to_owned(), "0.000032".to_owned())]),
        };
        registry.apply_catalog(std::slice::from_ref(&entry), "2026-10-11");

        let card = registry
            .card("openai/gpt-audio", ModelTask::Transcription)
            .expect("card");
        assert_eq!(card.display_name, "OpenAI: GPT Audio");
        assert_eq!(card.request_family, RequestFamily::Unknown);
        assert!(!card.verification.is_verified());
        assert!(!card.is_selectable());
        assert!(card.pricing.audio_input_per_token.is_known());
        assert_eq!(registry.source(), CatalogSource::Live);
        assert_eq!(registry.catalog_fetched_at(), Some("2026-10-11"));
    }

    #[test]
    fn a_catalog_entry_without_audio_downgrades_a_transcription_candidate() {
        let mut registry = registry();
        let entry = CatalogEntry {
            id: "openai/gpt-audio".to_owned(),
            name: "Text only now".to_owned(),
            context_length: None,
            architecture: crate::catalog::Architecture {
                input_modalities: vec!["text".to_owned()],
                output_modalities: vec!["text".to_owned()],
            },
            pricing: BTreeMap::new(),
        };
        registry.apply_catalog(&[entry], "2026-10-12");
        let card = registry
            .card("openai/gpt-audio", ModelTask::Transcription)
            .expect("card");
        assert_eq!(card.request_family, RequestFamily::Unsupported);
        assert!(!card.is_selectable());
    }

    #[test]
    fn only_an_observed_exchange_with_a_resolved_route_verifies() {
        let mut registry = registry();
        let mut record = observed("openai/whisper-1");

        record.evidence = ProbeEvidence::CatalogOnly;
        registry.apply_probe(ModelTask::Transcription, &record, "fixture-a");
        assert!(!registry
            .card("openai/whisper-1", ModelTask::Transcription)
            .expect("card")
            .verification
            .is_verified());

        record.evidence = ProbeEvidence::ObservedExchange;
        record.provider_route = Known::Unknown;
        registry.apply_probe(ModelTask::Transcription, &record, "fixture-a");
        assert_eq!(
            registry
                .card("openai/whisper-1", ModelTask::Transcription)
                .expect("card")
                .verification,
            Verification::NotVerified {
                reason: NotVerifiedReason::PrivacyRouteUnresolved
            }
        );

        record.provider_route = Known::Known("OpenAI".to_owned());
        registry.apply_probe(ModelTask::Transcription, &record, "fixture-a");
        let card = registry
            .card("openai/whisper-1", ModelTask::Transcription)
            .expect("card");
        assert_eq!(
            card.verification,
            Verification::Verified {
                fixture_id: "fixture-a".to_owned(),
                validated_at: "2026-10-11".to_owned()
            }
        );
        assert!(card.is_selectable());
        assert_eq!(card.features.segments, Tri::Yes);
        assert_eq!(card.features.word_timestamps, Tri::No);
    }

    #[test]
    fn an_observed_family_without_a_builder_stays_unselectable() {
        let mut registry = registry();
        let mut record = observed("some/text-model");
        record.request_family = RequestFamily::Unsupported;
        registry.apply_probe(ModelTask::Transcription, &record, "fixture-a");
        let card = registry
            .card("some/text-model", ModelTask::Transcription)
            .expect("card");
        assert_eq!(
            card.verification,
            Verification::NotVerified {
                reason: NotVerifiedReason::NoRequestBuilder
            }
        );
        assert!(!card.is_selectable());
    }

    #[test]
    fn a_custom_model_id_is_always_unverified() {
        let mut registry = registry();
        let card = registry.add_custom("vendor/experimental", ModelTask::Transcription);
        assert_eq!(
            card.verification,
            Verification::NotVerified {
                reason: NotVerifiedReason::UserSuppliedCustomId
            }
        );
        assert!(!card.is_selectable());
    }

    #[test]
    fn a_later_catalog_merge_cannot_undo_or_create_a_verification() {
        let mut registry = registry();
        registry.apply_probe(
            ModelTask::Transcription,
            &observed("openai/whisper-1"),
            "fixture-a",
        );
        let entry = CatalogEntry {
            id: "openai/whisper-1".to_owned(),
            name: "Whisper".to_owned(),
            context_length: None,
            architecture: crate::catalog::Architecture {
                input_modalities: vec!["audio".to_owned()],
                output_modalities: vec!["text".to_owned()],
            },
            pricing: BTreeMap::new(),
        };
        registry.apply_catalog(&[entry], "2026-10-12");
        assert!(registry
            .card("openai/whisper-1", ModelTask::Transcription)
            .expect("card")
            .verification
            .is_verified());

        // And a catalog merge alone never verifies a different model.
        assert!(!registry
            .card("openai/gpt-audio", ModelTask::Transcription)
            .expect("card")
            .verification
            .is_verified());
    }

    #[test]
    fn a_resolved_destination_is_attached_to_the_card() {
        let mut registry = registry();
        let resolution = crate::privacy::resolve(
            "openai/gpt-audio",
            ContentType::AudioBytes,
            None,
            &[crate::privacy::DocumentedPolicy {
                model_id: "openai/gpt-audio".to_owned(),
                downstream_provider: None,
                content_type: ContentType::AudioBytes,
                posture: RetentionPosture::ZeroRetention,
            }],
        );
        registry.apply_privacy(ModelTask::Transcription, resolution);
        let card = registry
            .card("openai/gpt-audio", ModelTask::Transcription)
            .expect("card");
        assert_eq!(
            card.privacy.as_ref().expect("privacy").posture,
            RetentionPosture::ZeroRetention
        );
    }
}
