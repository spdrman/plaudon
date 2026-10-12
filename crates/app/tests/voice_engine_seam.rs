//! The voice-profile engine seam, exercised from outside the crate.
//!
//! A desktop integration test has to be able to run enrolment and matching
//! with no ONNX model file and no OS keychain. That is what
//! [`EngineConfig::with_voice_engines`] is for, and the observable proof that
//! it reached the backend is `get_voice_profile_settings`, which reports
//! whether an embedder is installed (VP-042): the feature answers honestly
//! that no model is present rather than pretending one could run.

use std::sync::Arc;

use plaudon_ai_diarization::{FakeDiarizationProvider, SpeakerTurn};
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{EmptyRequest, SetVoiceProfilesEnabledRequest, VoiceProfileSettingsDto};
use plaudon_app::notify::UnsupportedNotifier;
use plaudon_app::runtime::{AppEngine, EngineConfig};
use plaudon_app::secrets::InMemorySecretStore;
use plaudon_app_core::voice_profiles::embedder::ScriptedEmbedder;

/// Dimension of the scripted checkpoint; the arithmetic lives in app-core.
const VOICE_DIM: usize = 8;

/// An engine over a fresh library, with the voice engines installed or not.
fn start(voice: bool) -> (AppEngine, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = EngineConfig::new(dir.path().to_path_buf())
        .with_secrets(Arc::new(InMemorySecretStore::new()))
        .with_notifier(Arc::new(UnsupportedNotifier))
        .with_worker_threads(2);
    if voice {
        config = config.with_voice_engines(
            Arc::new(FakeDiarizationProvider::new(vec![
                SpeakerTurn::new("S1", 0, 12_000),
                SpeakerTurn::new("S2", 12_000, 24_000),
            ])),
            Arc::new(ScriptedEmbedder::new(VOICE_DIM)),
            Arc::new(plaudon_security::secrets::InMemorySecretStore::new()),
        );
    }
    let engine = AppEngine::start(config).expect("engine starts");
    (engine, dir)
}

/// Turn the feature on and read the settings back.
fn enabled_settings(engine: &AppEngine) -> VoiceProfileSettingsDto {
    let disclosure = match engine
        .dispatcher()
        .call_blocking(AppCommand::GetVoiceProfileSettings(EmptyRequest {}))
        .expect("settings are readable")
    {
        AppResponse::GetVoiceProfileSettings(settings) => settings.disclosure.version,
        other => panic!("unexpected response: {other:?}"),
    };
    match engine
        .dispatcher()
        .call_blocking(AppCommand::SetVoiceProfilesEnabled(
            SetVoiceProfilesEnabledRequest {
                enabled: true,
                accept_disclosure_version: Some(disclosure),
            },
        ))
        .expect("the feature turns on")
    {
        AppResponse::SetVoiceProfilesEnabled(settings) => settings,
        other => panic!("unexpected response: {other:?}"),
    }
}

#[test]
fn an_installed_embedder_is_reported_as_available() {
    let (engine, _dir) = start(true);
    let settings = enabled_settings(&engine);
    assert!(settings.enabled);
    assert!(
        settings.embedder_available,
        "the scripted embedder reached the backend: {settings:?}"
    );
}

#[test]
fn a_configuration_without_voice_engines_reports_no_embedder() {
    let (engine, _dir) = start(false);
    let settings = enabled_settings(&engine);
    assert!(settings.enabled);
    assert!(
        !settings.embedder_available,
        "this build ships no embedder, and says so: {settings:?}"
    );
}
