//! Issue #303: a download or a dependency install reaches the network only
//! under a grant for that one action — one named host, one named file — that
//! Local-only Mode refuses outright, that is recorded in the consent log, and
//! that is revoked as soon as the action ends.

use std::path::Path;
use std::sync::Arc;

use plaudon_security::consent::{ConsentPurpose, GrantScope};
use plaudon_security::outbound::action::{ActionApproval, ActionEgressGrant, ActionScope};
use plaudon_security::outbound::subprocess::{
    GatedProcessLauncher, SpawnError, NPM_REGISTRY_HOST, PLAUD_CLI_PACKAGE,
};
use plaudon_security::outbound::{
    DenyReason, EgressPurpose, OutboundPolicy, PayloadClass, Subsystem,
};

const URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/\
5359861c739e955e79d9a303bcbc70fb988958b1/ggml-tiny.en.bin";
const FILE: &str = "/ggerganov/whisper.cpp/resolve/\
5359861c739e955e79d9a303bcbc70fb988958b1/ggml-tiny.en.bin";

fn download_scope() -> ActionScope {
    ActionScope::model_download(URL, Some("hf.co")).expect("a valid catalogue url")
}

fn approval() -> ActionApproval {
    ActionApproval {
        host: "huggingface.co".to_owned(),
        resource: FILE.to_owned(),
    }
}

fn approved() -> ActionEgressGrant {
    ActionEgressGrant::approve(false, download_scope(), &approval(), "grant-1", 1_000)
        .expect("an approved download outside Local-only Mode")
}

#[test]
fn issue_303_a_download_scope_names_exactly_one_host_and_one_file() {
    let scope = download_scope();
    assert_eq!(scope.host, "huggingface.co");
    assert_eq!(scope.resource, FILE);
    assert_eq!(scope.subsystem, Subsystem::ModelDistribution);
    assert_eq!(scope.purpose, EgressPurpose::ModelDownload);
    assert_eq!(scope.consent_purpose, ConsentPurpose::ModelDownload);
    assert_eq!(scope.delivery_host_suffix.as_deref(), Some("hf.co"));
}

#[test]
fn issue_303_local_only_mode_refuses_the_grant_itself() {
    let error = ActionEgressGrant::approve(true, download_scope(), &approval(), "grant-1", 1_000)
        .expect_err("Local-only Mode must refuse a download even when approved");
    assert_eq!(error.reason, DenyReason::LocalOnlyMode);
}

#[test]
fn issue_303_an_approval_for_a_different_file_or_host_is_refused() {
    let other_file = ActionApproval {
        host: "huggingface.co".to_owned(),
        resource: "/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin".to_owned(),
    };
    let error = ActionEgressGrant::approve(false, download_scope(), &other_file, "g", 1)
        .expect_err("approving another file does not approve this one");
    assert_eq!(error.reason, DenyReason::ActionNotApproved);

    let other_host = ActionApproval {
        host: "example.org".to_owned(),
        resource: FILE.to_owned(),
    };
    let error = ActionEgressGrant::approve(false, download_scope(), &other_host, "g", 1)
        .expect_err("approving another host does not approve this one");
    assert_eq!(error.reason, DenyReason::ActionNotApproved);
}

#[test]
fn issue_303_the_grant_authorizes_only_the_approved_url() {
    let grant = approved();
    let ticket = grant.authorize_url(URL).expect("the approved file");
    assert_eq!(ticket.subsystem(), Subsystem::ModelDistribution);
    assert_eq!(ticket.purpose(), EgressPurpose::ModelDownload);
    assert_eq!(ticket.payload(), PayloadClass::None);

    for refused in [
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin",
        "https://huggingface.co/api/models",
        "https://evil.example/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-tiny.en.bin",
        "http://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-tiny.en.bin",
        "https://openrouter.ai/api/v1/models",
    ] {
        assert!(
            grant.authorize_url(refused).is_err(),
            "{refused} must not be authorized by a grant for one file"
        );
    }
}

#[test]
fn issue_303_redirects_may_only_land_on_the_disclosed_delivery_domain() {
    let grant = approved();
    grant
        .authorize_redirect("https://us.aws.cdn.hf.co/xet-bridge-us/abc?sig=1")
        .expect("the disclosed delivery domain");
    grant
        .authorize_redirect("https://cas-bridge.xethub.hf.co/x")
        .expect("any server under the disclosed delivery domain");
    for refused in [
        "https://evil.example/x",
        "https://hf.co.evil.example/x",
        "https://nothf.co/x",
        "http://us.aws.cdn.hf.co/x",
        "https://127.0.0.1/x",
    ] {
        assert!(
            grant.authorize_redirect(refused).is_err(),
            "{refused} must not be followed"
        );
    }
}

#[test]
fn issue_303_the_grant_is_recorded_in_the_consent_log_and_covers_no_dispatch() {
    let grant = approved();
    let record = grant.consent_record();
    assert_eq!(record.id, "grant-1");
    assert_eq!(record.purpose, ConsentPurpose::ModelDownload);
    assert_eq!(record.granted_at_unix, 1_000);
    assert_eq!(
        record.scope,
        GrantScope::AskEveryTime,
        "it must cover nothing else"
    );
    let route = record
        .routes
        .iter()
        .next()
        .expect("the host and file are recorded");
    assert_eq!(route.relay, "huggingface.co");
    assert_eq!(route.model_id, FILE);
    assert!(
        record.content_types.is_empty(),
        "a download sends no user content"
    );
    assert!(!record.is_revoked());
}

#[test]
fn issue_303_the_grant_is_revoked_when_the_action_ends() {
    let grant = approved();
    let record = grant.revoke(2_000);
    assert_eq!(record.revoked_at_unix, Some(2_000));
    assert!(grant.is_revoked());
    assert_eq!(grant.consent_record().revoked_at_unix, Some(2_000));
    let error = grant
        .authorize_url(URL)
        .expect_err("nothing is authorized once the action has ended");
    assert_eq!(error.reason, DenyReason::ActionNotApproved);
    assert!(grant
        .authorize_redirect("https://us.aws.cdn.hf.co/x")
        .is_err());
    assert!(grant.launcher().is_err());
}

#[test]
fn issue_303_a_dependency_install_is_scoped_to_the_registry_and_the_package() {
    let scope = ActionScope::dependency_install(NPM_REGISTRY_HOST, PLAUD_CLI_PACKAGE);
    assert_eq!(scope.host, "registry.npmjs.org");
    assert_eq!(scope.resource, "@plaud-ai/cli@0.3.15");
    assert_eq!(scope.subsystem, Subsystem::PlaudCloudCli);
    assert_eq!(scope.purpose, EgressPurpose::AssetDownload);
    assert_eq!(scope.consent_purpose, ConsentPurpose::DependencyInstall);
    assert_eq!(scope.delivery_host_suffix, None);
}

#[test]
fn issue_303_the_guided_cli_install_is_npm_with_literal_arguments_only() {
    let scope = ActionScope::dependency_install(NPM_REGISTRY_HOST, PLAUD_CLI_PACKAGE);
    let approval = ActionApproval {
        host: NPM_REGISTRY_HOST.to_owned(),
        resource: PLAUD_CLI_PACKAGE.to_owned(),
    };
    let grant = ActionEgressGrant::approve(false, scope, &approval, "install-1", 5).unwrap();
    let launcher = grant
        .launcher()
        .expect("an approved install gets a launcher");
    let command = launcher
        .prepare_cli_install(Path::new("/usr/local/bin/npm"))
        .expect("npm is the pinned install program");
    let args: Vec<String> = command
        .args()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args,
        [
            "install",
            "--global",
            "--registry=https://registry.npmjs.org/",
            "@plaud-ai/cli@0.3.15"
        ]
    );
    assert_eq!(command.program(), Path::new("/usr/local/bin/npm"));
    assert_eq!(command.ticket().purpose(), EgressPurpose::AssetDownload);
    assert!(
        !command.env().contains_key("NODE_OPTIONS"),
        "the install must not inherit code-loading variables"
    );
}

#[test]
fn issue_303_the_guided_cli_install_is_refused_in_local_only_mode() {
    let launcher = GatedProcessLauncher::new(Arc::new(OutboundPolicy::local_only()));
    let error = launcher
        .prepare_cli_install(Path::new("/usr/local/bin/npm"))
        .expect_err("Local-only Mode never launches an installer");
    match error {
        SpawnError::Denied(denied) => assert_eq!(denied.reason, DenyReason::LocalOnlyMode),
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn issue_303_npm_cannot_be_launched_as_the_plaud_cli_and_vice_versa() {
    let scope = ActionScope::dependency_install(NPM_REGISTRY_HOST, PLAUD_CLI_PACKAGE);
    let approval = ActionApproval {
        host: NPM_REGISTRY_HOST.to_owned(),
        resource: PLAUD_CLI_PACKAGE.to_owned(),
    };
    let grant = ActionEgressGrant::approve(false, scope, &approval, "install-2", 5).unwrap();
    let launcher = grant.launcher().unwrap();
    // The installer is pinned to npm: handing it any other program is refused.
    assert!(launcher
        .prepare_cli_install(Path::new("/usr/local/bin/plaud"))
        .is_err());
    assert!(launcher.prepare_cli_install(Path::new("/bin/sh")).is_err());
}
