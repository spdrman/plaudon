//! Validated logic behind the `device_*` commands and `list_nearby_devices`
//! (spec §15.1, §15.2, §7.9).
//!
//! Every argument is validated here, in Rust, before a port is called. A
//! peripheral identifier is data, never a path or a command; a token is moved
//! straight into [`SecretValue`] and is never echoed, logged or serialized.

use plaudon_domain::error::AppError;
use plaudon_plaud_protocol::payload::MAX_RECORDING_ID_LEN;

use crate::dto::{
    DeviceAuthStatusDto, DeviceAuthorizeRequest, DeviceDisconnectDto, DeviceDisconnectRequest,
    DeviceIdRequest, DeviceImportDto, DeviceImportRequest, DeviceListingDto, DeviceProbeDto,
    DeviceProbeRequest, DeviceScanDto, DeviceSyncPolicyDto, DeviceSyncPolicyRequest,
    ListNearbyDevicesRequest,
};
use crate::error::invalid_argument;
use crate::secrets::SecretValue;
use crate::AppPorts;

/// Shortest accepted scan.
pub const MIN_SCAN_MS: u64 = 1_000;
/// Longest accepted scan.
pub const MAX_SCAN_MS: u64 = 30_000;
/// Longest accepted peripheral identifier.
pub const MAX_PERIPHERAL_ID_LEN: usize = 128;
/// Largest number of recordings in one `device_import`.
pub const MAX_IMPORT_RECORDINGS: usize = 64;
/// Longest accepted token, in hex characters (a 32-byte token is 64).
pub const MAX_TOKEN_HEX_LEN: usize = 128;

/// Validate a scan duration.
pub fn scan_timeout(timeout_ms: u64) -> Result<u64, AppError> {
    if (MIN_SCAN_MS..=MAX_SCAN_MS).contains(&timeout_ms) {
        Ok(timeout_ms)
    } else {
        Err(invalid_argument(format!(
            "timeout_ms {timeout_ms} is outside {MIN_SCAN_MS}..={MAX_SCAN_MS}"
        )))
    }
}

/// Validate a host-assigned peripheral identifier.
///
/// macOS reports a UUID, Windows a Bluetooth address and Linux a BlueZ object
/// name; all fit `[A-Za-z0-9:_./#{}-]`. Anything else (whitespace, control
/// characters, `..` path segments, shell metacharacters) is refused.
pub fn peripheral_id(value: &str) -> Result<&str, AppError> {
    if value.is_empty() || value.len() > MAX_PERIPHERAL_ID_LEN {
        return Err(invalid_argument("peripheral_id must be 1..=128 characters"));
    }
    if !value.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '.' | '/' | '#' | '{' | '}' | '-')
    }) {
        return Err(invalid_argument(
            "peripheral_id contains a forbidden character",
        ));
    }
    if value.split('/').any(|segment| segment == "..") {
        return Err(invalid_argument("peripheral_id contains a path segment"));
    }
    Ok(value)
}

/// Validate a device-scoped recording identifier.
pub fn device_recording_id(value: &str) -> Result<&str, AppError> {
    if value.is_empty() || value.len() > MAX_RECORDING_ID_LEN {
        return Err(invalid_argument(format!(
            "recording id must be 1..={MAX_RECORDING_ID_LEN} characters"
        )));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | ':'))
    {
        return Err(invalid_argument(
            "recording id contains a forbidden character",
        ));
    }
    Ok(value)
}

/// Validate the list of recordings to import: non-empty, bounded, unique.
pub fn recording_ids(values: &[String]) -> Result<Vec<String>, AppError> {
    if values.is_empty() {
        return Err(invalid_argument("recording_ids is empty"));
    }
    if values.len() > MAX_IMPORT_RECORDINGS {
        return Err(invalid_argument(format!(
            "recording_ids has more than {MAX_IMPORT_RECORDINGS} entries"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let id = device_recording_id(value)?;
        if !seen.insert(id) {
            return Err(invalid_argument("recording_ids contains a duplicate"));
        }
        out.push(id.to_owned());
    }
    Ok(out)
}

/// Validate a hex token and move it into a [`SecretValue`].
///
/// The error text never contains the offending value, only its shape.
pub fn token_secret(raw: String) -> Result<SecretValue, AppError> {
    if raw.len() > MAX_TOKEN_HEX_LEN + 64 {
        return Err(invalid_argument("the token is too long"));
    }
    let secret = SecretValue::new(raw)?;
    let hex = secret.expose();
    if hex.len() > MAX_TOKEN_HEX_LEN || hex.len() % 2 != 0 {
        return Err(invalid_argument(
            "the token must be an even number of hexadecimal characters",
        ));
    }
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(invalid_argument("the token must be hexadecimal"));
    }
    Ok(secret)
}

/// Scan for nearby devices.
pub async fn list_nearby_devices(
    ports: &AppPorts,
    request: ListNearbyDevicesRequest,
) -> Result<DeviceScanDto, AppError> {
    ports
        .devices
        .list_nearby(scan_timeout(request.timeout_ms)?)
        .await
}

/// Probe one device.
pub async fn device_probe(
    ports: &AppPorts,
    request: DeviceProbeRequest,
) -> Result<DeviceProbeDto, AppError> {
    let id = peripheral_id(&request.peripheral_id)?;
    ports
        .devices
        .probe(id, request.experimental_probe_approved)
        .await
}

/// Store a device credential. Returns status only.
pub async fn device_authorize(
    ports: &AppPorts,
    request: DeviceAuthorizeRequest,
) -> Result<DeviceAuthStatusDto, AppError> {
    let id = peripheral_id(&request.peripheral_id)?.to_owned();
    let token = token_secret(request.token_hex)?;
    ports.devices.authorize(&id, token).await
}

/// List the recordings a device holds.
pub async fn device_list_recordings(
    ports: &AppPorts,
    request: DeviceIdRequest,
) -> Result<DeviceListingDto, AppError> {
    let id = peripheral_id(&request.peripheral_id)?;
    ports.devices.list_recordings(id).await
}

/// Start read-only transfers of allowlisted recordings.
pub async fn device_import(
    ports: &AppPorts,
    request: DeviceImportRequest,
) -> Result<DeviceImportDto, AppError> {
    let id = peripheral_id(&request.peripheral_id)?;
    let ids = recording_ids(&request.recording_ids)?;
    let transfer_ids = ports.devices.import(id, ids, request.local_only).await?;
    Ok(DeviceImportDto { transfer_ids })
}

/// End the session; optionally forget the stored authorization.
pub async fn device_disconnect(
    ports: &AppPorts,
    request: DeviceDisconnectRequest,
) -> Result<DeviceDisconnectDto, AppError> {
    let id = peripheral_id(&request.peripheral_id)?;
    ports.devices.disconnect(id).await?;
    if request.forget_authorization {
        ports.devices.revoke(id).await?;
    }
    Ok(DeviceDisconnectDto {
        peripheral_id: id.to_owned(),
        local_archive_retained: true,
        authorization_revoked: request.forget_authorization,
    })
}

/// Opt in or out of read-only sync when the device is nearby.
pub async fn device_sync_policy(
    ports: &AppPorts,
    request: DeviceSyncPolicyRequest,
) -> Result<DeviceSyncPolicyDto, AppError> {
    let id = peripheral_id(&request.peripheral_id)?;
    ports
        .devices
        .sync_policy(id, request.enable_when_nearby)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use plaudon_domain::error::AppErrorCode;
    use std::path::PathBuf;
    use std::sync::Arc;

    use plaudon_notifications::testing::RecordingNotifier;
    use plaudon_notifications::{Dispatcher, NotificationSettings as CrateSettings, Notifier};
    use plaudon_source_plaud_device::{IdentityAssurance, SupportStatus, TransferState};
    use serde::Serialize;
    use serde_json::{json, Value};

    use crate::dto::{
        CloudRoute, DeviceCandidateDto, DeviceCapability, DeviceCapabilityDto, DeviceGateDto,
        DeviceRecordingDto, DeviceSection, DeviceTransferDto, ListingCompleteness, RadioState,
        RadioStatusDto,
    };
    use crate::error::to_command_result;
    use crate::events::{EventSink, NullEventSink};
    use crate::secrets::{InMemorySecretStore, SecretStore};

    fn code(result: Result<impl std::fmt::Debug, AppError>) -> AppErrorCode {
        result.expect_err("expected a refusal").code()
    }

    #[test]
    fn scan_timeouts_outside_the_bounds_are_refused() {
        assert_eq!(scan_timeout(MIN_SCAN_MS), Ok(MIN_SCAN_MS));
        assert_eq!(scan_timeout(MAX_SCAN_MS), Ok(MAX_SCAN_MS));
        for bad in [0, MIN_SCAN_MS - 1, MAX_SCAN_MS + 1, u64::MAX] {
            assert_eq!(code(scan_timeout(bad)), AppErrorCode::InvalidArgument);
        }
    }

    #[test]
    fn real_peripheral_identifiers_pass_and_hostile_ones_do_not() {
        for good in [
            "AABBCCDD-0011-2233-4455-667788990011",
            "AA:BB:CC:DD:EE:FF",
            "hci0/dev_AA_BB_CC_DD_EE_FF",
            "BluetoothLE#BluetoothLEaa:bb-cc:dd",
        ] {
            assert_eq!(peripheral_id(good), Ok(good), "{good}");
        }
        for bad in [
            "",
            " ",
            "a b",
            "id;rm -rf /",
            "id\n",
            "../etc/passwd",
            "hci0/../dev",
            "id\u{202e}txt",
            "id$(whoami)",
            &"a".repeat(MAX_PERIPHERAL_ID_LEN + 1),
        ] {
            assert_eq!(
                code(peripheral_id(bad)),
                AppErrorCode::InvalidArgument,
                "{bad:?}"
            );
        }
    }

    #[test]
    fn recording_ids_must_be_non_empty_bounded_unique_and_plain() {
        let ok = recording_ids(&["rec-1".into(), "2026.01.02_x:3".into()]).unwrap();
        assert_eq!(ok.len(), 2);
        assert_eq!(
            code(recording_ids(&[])),
            AppErrorCode::InvalidArgument,
            "empty"
        );
        assert_eq!(
            code(recording_ids(&["a".into(), "a".into()])),
            AppErrorCode::InvalidArgument,
            "duplicate"
        );
        assert_eq!(
            code(recording_ids(&["a b".into()])),
            AppErrorCode::InvalidArgument,
            "space"
        );
        assert_eq!(
            code(recording_ids(&["x".repeat(MAX_RECORDING_ID_LEN + 1)])),
            AppErrorCode::InvalidArgument,
            "too long"
        );
        let many: Vec<String> = (0..=MAX_IMPORT_RECORDINGS)
            .map(|n| format!("r{n}"))
            .collect();
        assert_eq!(
            code(recording_ids(&many)),
            AppErrorCode::InvalidArgument,
            "too many"
        );
    }

    #[test]
    fn a_token_must_be_even_length_hex_and_the_error_never_contains_it() {
        let good = "00112233445566778899aabbccddeeff";
        assert_eq!(token_secret(good.into()).unwrap().expose(), good);

        for bad in [
            "",
            "   ",
            "abc",
            "zz112233445566778899aabbccddeeff",
            "0011 2233",
            "00\n11",
        ] {
            let err = token_secret(bad.into()).err().expect("refused");
            assert_eq!(err.code(), AppErrorCode::InvalidArgument, "{bad:?}");
            let rendered = format!("{err:?} {err}");
            if !bad.trim().is_empty() {
                assert!(!rendered.contains(bad.trim()), "the error echoed the token");
            }
        }
        let too_long = "ab".repeat(MAX_TOKEN_HEX_LEN);
        assert_eq!(
            token_secret(too_long).err().expect("refused").code(),
            AppErrorCode::InvalidArgument
        );
    }
    /* --------------------------------------------- command layer, real wiring */

    const TOKEN: &str = "00112233445566778899aabbccddeeff";

    async fn ports() -> (AppPorts, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let notifier = RecordingNotifier::granted();
        let dispatcher = Arc::new(Dispatcher::new(
            Arc::clone(&notifier) as Arc<dyn Notifier>,
            CrateSettings::uniform(false),
        ));
        let ports = crate::build_ports(
            dir.path(),
            Arc::new(NullEventSink) as Arc<dyn EventSink>,
            dispatcher,
            Arc::new(InMemorySecretStore::new()) as Arc<dyn SecretStore>,
            Vec::<PathBuf>::new(),
        )
        .await
        .expect("the library opens");
        (ports, dir)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_build_without_a_radio_answers_with_an_honest_radio_state() {
        let (ports, _dir) = ports().await;
        let scan = list_nearby_devices(&ports, ListNearbyDevicesRequest { timeout_ms: 5_000 })
            .await
            .expect("a scan request is valid");
        assert_eq!(scan.radio.state, RadioState::UnavailableInBuild);
        assert_eq!(scan.devices, Vec::new());
        assert!(scan.scanned_at.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_invalid_argument_becomes_the_error_envelope_and_never_reaches_the_port() {
        let (ports, _dir) = ports().await;
        let result = to_command_result(
            list_nearby_devices(&ports, ListNearbyDevicesRequest { timeout_ms: 5 }).await,
        );
        let envelope = result.expect_err("out-of-range timeout is refused");
        assert_eq!(envelope.code, AppErrorCode::InvalidArgument);
        assert_eq!(
            envelope.message,
            AppErrorCode::InvalidArgument.canonical_message()
        );

        for request in [
            DeviceProbeRequest {
                peripheral_id: "../../etc/passwd".into(),
                experimental_probe_approved: false,
            },
            DeviceProbeRequest {
                peripheral_id: "id; rm -rf /".into(),
                experimental_probe_approved: true,
            },
        ] {
            let envelope = to_command_result(device_probe(&ports, request).await)
                .expect_err("hostile identifier is refused");
            assert_eq!(envelope.code, AppErrorCode::InvalidArgument);
            let json = serde_json::to_string(&envelope).unwrap();
            assert!(
                !json.contains("passwd") && !json.contains("rm -rf"),
                "{json}"
            );
        }

        let envelope = to_command_result(
            device_import(
                &ports,
                DeviceImportRequest {
                    peripheral_id: "AA:BB".into(),
                    recording_ids: vec![],
                    local_only: true,
                },
            )
            .await,
        )
        .expect_err("an empty import is refused");
        assert_eq!(envelope.code, AppErrorCode::InvalidArgument);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn authorizing_with_a_malformed_token_returns_an_envelope_without_the_token() {
        let (ports, _dir) = ports().await;
        let bad = "not-hex-at-all-0123";
        let envelope = to_command_result(
            device_authorize(
                &ports,
                DeviceAuthorizeRequest {
                    peripheral_id: "AA:BB".into(),
                    token_hex: bad.into(),
                },
            )
            .await,
        )
        .expect_err("a malformed token is refused");
        assert_eq!(envelope.code, AppErrorCode::InvalidArgument);
        assert!(!serde_json::to_string(&envelope).unwrap().contains(bad));

        // A well-formed token for a device that was never probed is refused too.
        let envelope = to_command_result(
            device_authorize(
                &ports,
                DeviceAuthorizeRequest {
                    peripheral_id: "AA:BB".into(),
                    token_hex: TOKEN.into(),
                },
            )
            .await,
        )
        .expect_err("an unprobed device cannot be authorized");
        assert!(!serde_json::to_string(&envelope).unwrap().contains(TOKEN));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disconnecting_always_retains_the_local_archive() {
        let (ports, _dir) = ports().await;
        let response = device_disconnect(
            &ports,
            DeviceDisconnectRequest {
                peripheral_id: "AA:BB".into(),
                forget_authorization: true,
            },
        )
        .await
        .expect("disconnect is idempotent");
        assert!(response.local_archive_retained);
        assert!(response.authorization_revoked);

        let envelope = to_command_result(
            device_disconnect(
                &ports,
                DeviceDisconnectRequest {
                    peripheral_id: String::new(),
                    forget_authorization: false,
                },
            )
            .await,
        )
        .expect_err("an empty identifier is refused");
        assert_eq!(envelope.code, AppErrorCode::InvalidArgument);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_device_that_was_never_scanned_cannot_be_probed_through_the_command() {
        let (ports, _dir) = ports().await;
        let envelope = to_command_result(
            device_probe(
                &ports,
                DeviceProbeRequest {
                    peripheral_id: "AA:BB".into(),
                    experimental_probe_approved: true,
                },
            )
            .await,
        )
        .expect_err("fail closed");
        assert_eq!(envelope.code, AppErrorCode::InvalidArgument);
    }

    /* ------------------------------------------------------ allowlist drift */

    #[test]
    fn the_seven_spec_device_commands_are_exactly_the_registered_set() {
        let expected = [
            "list_nearby_devices",
            "device_probe",
            "device_authorize",
            "device_list_recordings",
            "device_import",
            "device_disconnect",
            "device_sync_policy",
        ];
        for name in expected {
            assert!(crate::command::COMMAND_NAMES.contains(&name), "{name}");
        }
        // No write-class device command exists: nothing deletes, clears,
        // controls recording, writes settings or acknowledges a sync.
        for name in crate::command::COMMAND_NAMES {
            if name.starts_with("device_") {
                assert!(expected.contains(name), "{name} is not a spec command");
                for banned in [
                    "delete", "clear", "erase", "ack_", "_ack", "control", "write", "ota",
                    "update", "firmware",
                ] {
                    assert!(!name.contains(banned), "{name} looks like a device write");
                }
            }
        }
    }

    #[test]
    fn the_published_typescript_contract_names_every_device_command_and_event() {
        // The TypeScript wire contract is the parity oracle until the React
        // frontend is deleted at cutover (EPIC O, O2.5); this assertion goes
        // with it.
        let contract = include_str!("../../../../apps/desktop/src/ipc/contract.ts");
        for name in [
            "list_nearby_devices",
            "device_probe",
            "device_authorize",
            "device_list_recordings",
            "device_import",
            "device_disconnect",
            "device_sync_policy",
        ] {
            assert!(
                contract.contains(&format!("{name}:")),
                "{name} is not in CommandMap"
            );
            assert!(
                contract.contains(&format!("\"{name}\"")),
                "{name} is not listed"
            );
        }
        for kind in crate::dto::EventKind::ALL {
            assert!(
                contract.contains(&format!("\"{}\"", kind.as_str())),
                "{} is not in EVENT_KINDS",
                kind.as_str()
            );
        }
    }

    /* ------------------------------------------------------ schema conformance */

    fn schema_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../schemas/commands/v1")
            .join(format!("{name}.schema.json"))
    }

    fn validate_against(name: &str, instance: &impl Serialize) {
        let raw = std::fs::read_to_string(schema_path(name)).expect("the schema exists");
        let schema: Value = serde_json::from_str(&raw).expect("valid json");
        let validator = jsonschema::validator_for(&schema).expect("the schema compiles");
        let value = serde_json::to_value(instance).expect("serializes");
        let errors: Vec<String> = validator
            .iter_errors(&value)
            .map(|error| format!("{error} at {}", error.instance_path))
            .collect();
        assert!(errors.is_empty(), "{name}: {}\n{value}", errors.join("\n"));
    }

    fn candidate() -> DeviceCandidateDto {
        DeviceCandidateDto {
            peripheral_id: "AA:BB".into(),
            section: DeviceSection::PreviouslyLinked,
            advertised_name: Some("PLAUD_NOTE".into()),
            rssi_dbm: Some(-60),
            transport: "ble".into(),
            model: None,
            firmware: None,
            support_status: SupportStatus::NotEvaluated,
            status_label: "Not evaluated".into(),
            identity: IdentityAssurance::Unverified,
            identity_label: "Unverified identity".into(),
            authorized: true,
            last_contact_at: None,
        }
    }

    fn transfer() -> DeviceTransferDto {
        DeviceTransferDto {
            transfer_id: None,
            state: TransferState::Discovered,
            state_label: "On device".into(),
            bytes_received: None,
            observed_bytes_per_sec: None,
            eta_seconds_estimate: None,
            verified: false,
            source_label: "Device BLE · compressed ASR".into(),
            cloud_route: CloudRoute::NotUsed,
        }
    }

    fn sync() -> crate::dto::DeviceSyncPolicyDto {
        crate::dto::DeviceSyncPolicyDto {
            peripheral_id: "AA:BB".into(),
            may_advertise: false,
            enabled: false,
            default_on: false,
            audit_code: "NOT_RUN".into(),
            explanation: "not audited".into(),
        }
    }

    #[test]
    fn every_device_request_and_response_matches_its_published_schema() {
        validate_against(
            "list_nearby_devices.request",
            &ListNearbyDevicesRequest { timeout_ms: 5_000 },
        );
        validate_against(
            "list_nearby_devices.response",
            &crate::dto::DeviceScanDto {
                radio: RadioStatusDto {
                    state: RadioState::UnavailableInBuild,
                    code: Some("SOURCE_UNAVAILABLE".into()),
                    message: "m".into(),
                },
                devices: vec![candidate()],
                scanned_at: None,
            },
        );
        validate_against(
            "device_probe.request",
            &DeviceProbeRequest {
                peripheral_id: "AA:BB".into(),
                experimental_probe_approved: false,
            },
        );
        validate_against(
            "device_probe.response",
            &crate::dto::DeviceProbeDto {
                peripheral_id: "AA:BB".into(),
                advertised_name: None,
                transport: "ble".into(),
                model: Some("Note".into()),
                firmware: None,
                auth_mode: None,
                battery_percent: None,
                rssi_dbm: None,
                section: DeviceSection::UnsupportedFirmware,
                support_status: SupportStatus::KnownUnsupported,
                status_label: "Known unsupported".into(),
                known_limitations: vec!["x".into()],
                gate: DeviceGateDto {
                    permits_transfer: false,
                    experimental_probe_available: false,
                    experimental_probe_approved: false,
                    refusal_code: Some("KNOWN_UNSUPPORTED".into()),
                },
                identity: IdentityAssurance::Unverified,
                identity_label: "Unverified identity".into(),
                capabilities: vec![DeviceCapabilityDto {
                    capability: DeviceCapability::AutomaticSync,
                    available: false,
                    reason: Some("r".into()),
                }],
                source_label: "Device BLE · quality not established".into(),
                cloud_route: CloudRoute::NotUsed,
                disclosures: vec![],
                provisioning_note: None,
                authorized: false,
                last_contact_at: Some("1970-01-01T00:00:00Z".into()),
                sync: sync(),
            },
        );
        // The request side of the secret-bearing command is checked from raw
        // JSON because `DeviceAuthorizeRequest` is deliberately not `Serialize`.
        let raw = std::fs::read_to_string(schema_path("device_authorize.request")).unwrap();
        let schema: Value = serde_json::from_str(&raw).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator
            .validate(&json!({"peripheral_id": "AA:BB", "token_hex": TOKEN}))
            .is_ok());
        assert!(validator
            .validate(&json!({"peripheral_id": "AA:BB", "token_hex": "xyz"}))
            .is_err());
        assert!(validator
            .validate(&json!({"peripheral_id": "AA:BB", "token_hex": TOKEN, "echo": true}))
            .is_err());
        validate_against(
            "device_authorize.response",
            &crate::dto::DeviceAuthStatusDto {
                peripheral_id: "AA:BB".into(),
                credential_stored: true,
                identity: IdentityAssurance::Unverified,
                identity_label: "Unverified identity".into(),
            },
        );
        validate_against(
            "device_list_recordings.request",
            &DeviceIdRequest {
                peripheral_id: "AA:BB".into(),
            },
        );
        validate_against(
            "device_list_recordings.response",
            &crate::dto::DeviceListingDto {
                peripheral_id: "AA:BB".into(),
                items: vec![DeviceRecordingDto {
                    recording_id: "rec-1".into(),
                    display_name: "n".into(),
                    declared_byte_length: 10,
                    captured_at: None,
                    transfer: transfer(),
                }],
                file_count: 1,
                completeness: ListingCompleteness::UnsyncedDelta,
                completeness_label: "l".into(),
                identity: IdentityAssurance::ProfileAuthenticated,
                identity_label: "Profile-authenticated".into(),
            },
        );
        validate_against(
            "device_import.request",
            &DeviceImportRequest {
                peripheral_id: "AA:BB".into(),
                recording_ids: vec!["rec-1".into()],
                local_only: true,
            },
        );
        validate_against(
            "device_import.response",
            &DeviceImportDto {
                transfer_ids: vec!["t".into()],
            },
        );
        validate_against(
            "device_disconnect.request",
            &DeviceDisconnectRequest {
                peripheral_id: "AA:BB".into(),
                forget_authorization: false,
            },
        );
        validate_against(
            "device_disconnect.response",
            &DeviceDisconnectDto {
                peripheral_id: "AA:BB".into(),
                local_archive_retained: true,
                authorization_revoked: false,
            },
        );
        validate_against(
            "device_sync_policy.request",
            &DeviceSyncPolicyRequest {
                peripheral_id: "AA:BB".into(),
                enable_when_nearby: false,
            },
        );
        validate_against("device_sync_policy.response", &sync());
    }

    #[test]
    fn a_renamed_or_added_device_field_fails_the_closed_schema() {
        let mut value = serde_json::to_value(candidate()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("token".into(), json!("leak"));
        let raw = std::fs::read_to_string(schema_path("list_nearby_devices.response")).unwrap();
        let schema: Value = serde_json::from_str(&raw).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let scan = json!({
            "radio": {"state": "scanned", "code": null, "message": "m"},
            "devices": [value],
            "scanned_at": null
        });
        assert!(validator.validate(&scan).is_err());
    }
}
