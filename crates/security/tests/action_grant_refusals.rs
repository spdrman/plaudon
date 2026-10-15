//! The refusal arms of a per-action egress grant (issue #303), asserted by
//! the reason each one reports (spec 18.9).
//!
//! `issue_303_action_grant.rs` covers the happy path and the headline
//! refusals. This file covers the rest: a catalogue URL that carries more than
//! a file, a grant asked to do the other kind of action, a redirect without a
//! disclosed delivery domain, the delivery domain itself, and a second
//! revocation that must not move the recorded end time.

use plaudon_security::outbound::action::{ActionApproval, ActionEgressGrant, ActionScope};
use plaudon_security::outbound::subprocess::{NPM_REGISTRY_HOST, PLAUD_CLI_PACKAGE};
use plaudon_security::outbound::{
    ApprovalSource, DenyReason, Destination, EgressPurpose, EgressRequest, NetworkGrant,
    OutboundPolicy, PayloadClass, Subsystem, PLAUD_CLI_INSTALLER_PROGRAM, PLAUD_CLI_PROGRAM,
};

const HOST: &str = "models.example.org";
const FILE: &str = "/whisper/ggml-tiny.en.bin";

fn url() -> String {
    format!("https://{HOST}{FILE}")
}

fn download(delivery: Option<&str>) -> ActionEgressGrant {
    let scope = ActionScope::model_download(&url(), delivery).expect("a plain file url");
    let approval = ActionApproval {
        host: HOST.to_ascii_uppercase(),
        resource: FILE.to_owned(),
    };
    ActionEgressGrant::approve(false, scope, &approval, "dl", 10)
        .expect("the host is compared without regard to case")
}

fn install() -> ActionEgressGrant {
    let scope = ActionScope::dependency_install(NPM_REGISTRY_HOST, PLAUD_CLI_PACKAGE);
    let approval = ActionApproval {
        host: NPM_REGISTRY_HOST.to_owned(),
        resource: PLAUD_CLI_PACKAGE.to_owned(),
    };
    ActionEgressGrant::approve(false, scope, &approval, "install", 10).expect("approved install")
}

#[test]
fn a_catalogue_url_that_names_more_than_one_file_is_not_a_download_scope() {
    for refused in [
        format!("https://{HOST}{FILE}?revision=main"),
        format!("https://{HOST}{FILE}#part-2"),
        format!("https://{HOST}/"),
        format!("https://{HOST}"),
    ] {
        let error = ActionScope::model_download(&refused, None)
            .expect_err("only a bare file path can be approved");
        assert_eq!(error.reason, DenyReason::MalformedDestination, "{refused}");
        assert_eq!(error.subsystem, Subsystem::ModelDistribution, "{refused}");
        assert_eq!(error.user_action, None, "{refused}");
    }

    // A URL the gate refuses outright never gets as far as the file check.
    let error = ActionScope::model_download(&format!("http://{HOST}{FILE}"), None)
        .expect_err("plain http is never a download scope");
    assert_ne!(error.reason, DenyReason::ActionNotApproved);

    // The delivery domain is stored lowercased, as hosts are compared.
    let scope = ActionScope::model_download(&url(), Some("CDN.Example.ORG")).unwrap();
    assert_eq!(
        scope.delivery_host_suffix.as_deref(),
        Some("cdn.example.org")
    );
}

#[test]
fn the_approved_path_with_a_query_or_fragment_is_not_the_approved_file() {
    let grant = download(None);
    grant
        .authorize_url(&url())
        .expect("the approved file itself");
    for refused in [format!("{}?token=1", url()), format!("{}#frag", url())] {
        let error = grant
            .authorize_url(&refused)
            .expect_err("same path, different request");
        assert_eq!(error.reason, DenyReason::ActionNotApproved, "{refused}");
    }
}

#[test]
fn an_install_grant_authorizes_no_url_and_no_redirect() {
    let grant = install();
    let error = grant
        .authorize_url(&format!("https://{NPM_REGISTRY_HOST}/@plaud-ai/cli"))
        .expect_err("an install grant is not a download grant");
    assert_eq!(error.reason, DenyReason::ActionNotApproved);
    assert_eq!(error.subsystem, Subsystem::PlaudCloudCli);

    let error = grant
        .authorize_redirect(&format!("https://{NPM_REGISTRY_HOST}/x"))
        .expect_err("an install grant follows no redirect");
    assert_eq!(error.reason, DenyReason::ActionNotApproved);
}

#[test]
fn a_download_grant_hands_out_no_process_launcher() {
    let error = download(None)
        .launcher()
        .expect_err("a download grant cannot run an installer");
    assert_eq!(error.reason, DenyReason::ActionNotApproved);
    assert_eq!(error.subsystem, Subsystem::ModelDistribution);
}

#[test]
fn without_a_disclosed_delivery_domain_a_redirect_must_stay_on_the_host() {
    let grant = download(None);
    grant
        .authorize_redirect(&format!("https://{HOST}/mirror/ggml-tiny.en.bin"))
        .expect("the approved host itself");
    let error = grant
        .authorize_redirect("https://cdn.example.org/ggml-tiny.en.bin")
        .expect_err("no delivery domain was disclosed");
    assert_eq!(error.reason, DenyReason::ActionNotApproved);
}

#[test]
fn the_delivery_domain_itself_is_an_accepted_redirect() {
    let grant = download(Some("cdn.example.org"));
    grant
        .authorize_redirect("https://cdn.example.org/blob")
        .expect("the disclosed domain itself, not only its servers");
    grant
        .authorize_redirect("https://eu.cdn.example.org/blob")
        .expect("a server under the disclosed domain");
    let error = grant
        .authorize_redirect("https://evilcdn.example.org/blob")
        .expect_err("a lookalike that only shares the trailing letters");
    assert_eq!(error.reason, DenyReason::ActionNotApproved);
}

#[test]
fn a_second_revocation_keeps_the_first_end_time() {
    let grant = download(None);
    assert_eq!(grant.revoke(20).revoked_at_unix, Some(20));
    let again = grant.revoke(99);
    assert_eq!(again.revoked_at_unix, Some(20));
    assert!(grant.is_revoked());
    assert_eq!(grant.id(), "dl");
    assert_eq!(grant.scope().host, HOST);
}

/// The policy half of the guided install: the dependency-install approval and
/// the pinned installer program are both required, and neither stands in for
/// the other.
#[test]
fn the_cli_install_purpose_needs_its_own_approval_and_the_pinned_installer() {
    let request = |program: &str| {
        EgressRequest::new(
            Subsystem::PlaudCloudCli,
            Destination::subprocess(program),
            EgressPurpose::AssetDownload,
            PayloadClass::None,
        )
    };
    let grant = || {
        NetworkGrant::new(ApprovalSource::TestHarness, 1_700_000_000, "privacy-v1")
            .with_allowed_hosts(vec![NPM_REGISTRY_HOST.to_owned()])
    };

    let unapproved = OutboundPolicy::network_permitted(grant());
    let denied = unapproved
        .authorize(&request(PLAUD_CLI_INSTALLER_PROGRAM))
        .expect_err("network access alone never installs anything");
    assert_eq!(denied.reason, DenyReason::ActionNotApproved);
    assert_eq!(denied.user_action, None);

    let approved =
        OutboundPolicy::network_permitted(grant().with_dependency_install_approved(true));
    for program in ["npm", "/usr/local/bin/npm", "npm.exe"] {
        approved
            .authorize(&request(program))
            .unwrap_or_else(|e| panic!("{program} is the pinned installer: {e:?}"));
    }
    for program in [PLAUD_CLI_PROGRAM, "npm.cmd", "npx", "/bin/sh"] {
        let denied = approved
            .authorize(&request(program))
            .expect_err("only npm runs the guided install");
        assert_eq!(
            denied.reason,
            DenyReason::DestinationNotAllowlisted,
            "{program}"
        );
    }

    // The approval does not widen what the CLI itself may be launched as.
    let denied = approved
        .authorize(&EgressRequest::new(
            Subsystem::PlaudCloudCli,
            Destination::subprocess(PLAUD_CLI_INSTALLER_PROGRAM),
            EgressPurpose::AccountSync,
            PayloadClass::Metadata,
        ))
        .expect_err("npm is not the PLAUD CLI");
    assert_eq!(denied.reason, DenyReason::DestinationNotAllowlisted);
}
