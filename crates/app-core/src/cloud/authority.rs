//! The consent authority the OpenRouter clients dispatch through
//! (spec 4.4, 10.8, 16.3).
//!
//! `plaudon-ai-openrouter` deliberately owns no consent state: it asks a
//! [`CloudDispatchAuthority`] for permission and cannot construct one. This is
//! the production implementation, composed from the EPIC D egress gate, the
//! EPIC D consent grants and the documented route policies EPIC G stores.

use std::sync::Arc;

use async_trait::async_trait;
use plaudon_ai_openrouter::client::{CloudDispatchAuthority, DispatchRefused};
use plaudon_ai_openrouter::privacy::{self, DocumentedPolicy, PrivacyResolution};
use plaudon_security::consent::{
    dispatch_preflight, ConsentError, ConsentGrant, ConsentPurpose, ContentType, DispatchApproval,
    DispatchRequest, RetentionPosture,
};
use plaudon_security::outbound::OutboundPolicy;

/// Everything a dispatch decision needs, assembled once per job.
#[derive(Debug, Clone)]
pub struct DispatchContext {
    /// Recording the work belongs to. Opaque id, never a title.
    pub recording_id: String,
    /// Disclosure version the app would show today (spec 4.4(5)).
    pub disclosure_version: String,
    /// Retention posture the user demanded, when they demanded one.
    pub required_retention: Option<RetentionPosture>,
    /// Documented policies for exact routes.
    pub documented_policies: Vec<DocumentedPolicy>,
    /// Route metadata fetched from the relay, when discovery succeeded.
    pub endpoints: Option<plaudon_ai_openrouter::privacy::EndpointsResponse>,
}

/// The production authority.
#[derive(Debug)]
pub struct ConsentAuthority {
    policy: Arc<OutboundPolicy>,
    grants: Vec<ConsentGrant>,
    context: DispatchContext,
    now_unix: i64,
}

impl ConsentAuthority {
    /// Assemble an authority for one job.
    #[must_use]
    pub fn new(
        policy: Arc<OutboundPolicy>,
        grants: Vec<ConsentGrant>,
        context: DispatchContext,
        now_unix: i64,
    ) -> Self {
        Self {
            policy,
            grants,
            context,
            now_unix,
        }
    }

    /// Resolve the destination and posture for one dispatch.
    #[must_use]
    pub fn resolve(&self, model_id: &str, content_type: ContentType) -> PrivacyResolution {
        privacy::resolve(
            model_id,
            content_type,
            self.context.endpoints.as_ref(),
            &self.context.documented_policies,
        )
    }

    /// The purpose a content type implies.
    #[must_use]
    pub const fn purpose_for(content_type: ContentType) -> ConsentPurpose {
        match content_type {
            ContentType::AudioBytes => ConsentPurpose::Transcription,
            ContentType::TranscriptText | ContentType::MetadataOnly => {
                ConsentPurpose::Summarization
            }
        }
    }
}

#[async_trait]
impl CloudDispatchAuthority for ConsentAuthority {
    async fn authorize(
        &self,
        content_type: ContentType,
        model_id: &str,
    ) -> Result<DispatchApproval, DispatchRefused> {
        let resolution = self.resolve(model_id, content_type);

        // Fail closed before the preflight when the user required a posture
        // that did not resolve for this exact route and content type.
        if privacy::enforce(&resolution, self.context.required_retention).is_err() {
            return Err(DispatchRefused::PrivacyUnresolved);
        }

        let mut request = DispatchRequest::new(
            Self::purpose_for(content_type),
            content_type,
            resolution.route.clone(),
            self.context.recording_id.clone(),
            self.context.disclosure_version.clone(),
        )
        .requiring_privacy_restriction(self.context.required_retention.is_some());
        if resolution.posture.is_resolved() {
            request = request.with_resolved_retention(resolution.posture);
        }

        dispatch_preflight(&self.policy, &self.grants, &request, self.now_unix).map_err(|error| {
            match error {
                ConsentError::Egress(_) => DispatchRefused::Egress,
                ConsentError::PrivacyPostureUnresolved { .. } => DispatchRefused::PrivacyUnresolved,
                ConsentError::Required { .. }
                | ConsentError::ScopeMismatch { .. }
                | ConsentError::Revoked { .. }
                | ConsentError::Expired { .. }
                | ConsentError::DisclosureChanged { .. } => DispatchRefused::ConsentRequired,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plaudon_security::consent::{ProviderRoute, RELAY_OPENROUTER};
    use plaudon_security::outbound::{ApprovalSource, NetworkGrant};

    const NOW: i64 = 1_793_000_000;
    const DISCLOSURE: &str = "2026-10-11";

    fn permitted() -> Arc<OutboundPolicy> {
        Arc::new(OutboundPolicy::network_permitted(NetworkGrant::new(
            ApprovalSource::TestHarness,
            NOW - 100,
            DISCLOSURE,
        )))
    }

    fn grant(model_id: &str) -> ConsentGrant {
        ConsentGrant::for_model_policy(
            "consent-1",
            ProviderRoute::new(RELAY_OPENROUTER, model_id),
            ConsentPurpose::Transcription,
            DISCLOSURE,
            NOW - 50,
        )
        .with_content_type(ContentType::AudioBytes)
    }

    fn context() -> DispatchContext {
        DispatchContext {
            recording_id: "recording-1".to_owned(),
            disclosure_version: DISCLOSURE.to_owned(),
            required_retention: None,
            documented_policies: Vec::new(),
            endpoints: None,
        }
    }

    #[tokio::test]
    async fn a_covering_grant_authorizes_one_dispatch() {
        let authority = ConsentAuthority::new(permitted(), vec![grant("m")], context(), NOW);
        let approval = authority
            .authorize(ContentType::AudioBytes, "m")
            .await
            .expect("authorized");
        assert_eq!(approval.grant_id, "consent-1");
    }

    #[tokio::test]
    async fn local_only_mode_refuses_before_consent_is_considered() {
        let authority = ConsentAuthority::new(
            Arc::new(OutboundPolicy::local_only()),
            vec![grant("m")],
            context(),
            NOW,
        );
        assert_eq!(
            authority
                .authorize(ContentType::AudioBytes, "m")
                .await
                .expect_err("refused"),
            DispatchRefused::Egress
        );
    }

    #[tokio::test]
    async fn a_revoked_grant_blocks_the_next_dispatch_immediately() {
        let mut revoked = grant("m");
        revoked.revoked_at_unix = Some(NOW - 1);
        let authority = ConsentAuthority::new(permitted(), vec![revoked], context(), NOW);
        assert_eq!(
            authority
                .authorize(ContentType::AudioBytes, "m")
                .await
                .expect_err("refused"),
            DispatchRefused::ConsentRequired
        );
    }

    #[tokio::test]
    async fn a_grant_for_another_model_does_not_cover_this_route() {
        let authority = ConsentAuthority::new(permitted(), vec![grant("other")], context(), NOW);
        assert_eq!(
            authority
                .authorize(ContentType::AudioBytes, "m")
                .await
                .expect_err("refused"),
            DispatchRefused::ConsentRequired
        );
    }

    #[tokio::test]
    async fn requiring_zero_retention_fails_closed_when_nothing_resolves() {
        let mut context = context();
        context.required_retention = Some(RetentionPosture::ZeroRetention);
        let authority = ConsentAuthority::new(permitted(), vec![grant("m")], context, NOW);
        assert_eq!(
            authority
                .authorize(ContentType::AudioBytes, "m")
                .await
                .expect_err("refused"),
            DispatchRefused::PrivacyUnresolved
        );
    }

    #[tokio::test]
    async fn a_documented_zero_retention_policy_for_the_exact_route_is_accepted() {
        let mut context = context();
        context.required_retention = Some(RetentionPosture::ZeroRetention);
        context.documented_policies = vec![DocumentedPolicy {
            model_id: "m".to_owned(),
            downstream_provider: None,
            content_type: ContentType::AudioBytes,
            posture: RetentionPosture::ZeroRetention,
        }];
        let authority = ConsentAuthority::new(
            permitted(),
            vec![grant("m").with_required_retention(RetentionPosture::ZeroRetention)],
            context,
            NOW,
        );
        assert!(authority
            .authorize(ContentType::AudioBytes, "m")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn a_transcript_policy_does_not_authorize_an_audio_upload() {
        let mut context = context();
        context.required_retention = Some(RetentionPosture::ZeroRetention);
        context.documented_policies = vec![DocumentedPolicy {
            model_id: "m".to_owned(),
            downstream_provider: None,
            content_type: ContentType::TranscriptText,
            posture: RetentionPosture::ZeroRetention,
        }];
        let authority = ConsentAuthority::new(permitted(), vec![grant("m")], context, NOW);
        assert_eq!(
            authority
                .authorize(ContentType::AudioBytes, "m")
                .await
                .expect_err("refused"),
            DispatchRefused::PrivacyUnresolved
        );
    }
}
