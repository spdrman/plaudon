//! Acceptance table for every secret shape the redactor must destroy,
//! including the hostile variants (spec §16.2, §18.5, App. B.5, App. B.7).

#![allow(clippy::unwrap_used, clippy::panic)]

use plaudon_diagnostics::patterns::MARKER_PREFIXES;
use plaudon_diagnostics::Redactor;

struct Case {
    name: &'static str,
    input: &'static str,
    /// Substring that must not survive.
    secret: &'static str,
    /// Marker that must appear.
    marker: &'static str,
}

const OPENROUTER_KEY: &str =
    "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";

const CASES: &[Case] = &[
    Case {
        name: "openrouter_key_in_message",
        input: "dispatching with key sk-or-v1-9f3c2b1a0d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f6",
        secret: "sk-or-v1-9f3c2b1a",
        marker: "<redacted:openrouter_key>",
    },
    Case {
        name: "openrouter_key_uppercase",
        input: "KEY=SK-OR-V1-9F3C2B1A0D4E5F60718293A4B5C6D7E8",
        secret: "SK-OR-V1-9F3C",
        marker: "<redacted:openrouter_key>",
    },
    Case {
        name: "openrouter_key_percent_encoded",
        input: "redirect=%73k-or-v1-9f3c2b1a0d4e5f60718293a4b5c6d7e8",
        secret: "k-or-v1-9f3c2b1a",
        marker: "<redacted:openrouter_key>",
    },
    Case {
        name: "openrouter_key_in_json_value",
        input: r#"{"provider":"openrouter","api_key":"sk-or-v1-9f3c2b1a0d4e5f60718293a4b5c6d7e8"}"#,
        secret: "sk-or-v1-9f3c2b1a",
        marker: "<redacted:openrouter_key>",
    },
    Case {
        name: "openrouter_key_split_across_adjacent_fields",
        // Neither half matches on its own; only the compacted view catches it.
        input: r#"head="sk-or-v1-9f3c2b1a" tail="0d4e5f60718293a4b5c6d7e8""#,
        secret: "sk-or-v1-9f3c2b1a",
        marker: "<redacted:openrouter_key>",
    },
    Case {
        name: "generic_sk_api_key",
        input: "provider key sk-proj-AbCdEf0123456789XyZwVuTs42",
        secret: "sk-proj-AbCdEf0123456789",
        marker: "<redacted:api_key>",
    },
    Case {
        name: "authorization_header",
        input: "GET /v1/models Authorization: Bearer abcDEF1234567890token",
        secret: "abcDEF1234567890token",
        marker: "<redacted:bearer>",
    },
    Case {
        name: "bare_bearer_token",
        input: "retrying with Bearer abcDEF1234567890token",
        secret: "abcDEF1234567890token",
        marker: "<redacted:bearer>",
    },
    Case {
        name: "authorization_lowercase_json",
        input: r#"{"authorization":"Bearer abcDEF1234567890token"}"#,
        secret: "abcDEF1234567890token",
        marker: "<redacted:bearer>",
    },
    Case {
        name: "token_parameter",
        input: "cli invoked with token=Zm9vYmFyYmF6cXV4 for session",
        secret: "Zm9vYmFyYmF6cXV4",
        marker: "<redacted:credential>",
    },
    Case {
        name: "signed_url_aws_sigv4",
        input: "download https://plaud-media.s3.amazonaws.com/u/42.wav?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Signature=4f1c9d2e8b7a6150&X-Amz-Expires=900 ok",
        secret: "X-Amz-Signature",
        marker: "<redacted:signed_url host=plaud-media.s3.amazonaws.com>",
    },
    Case {
        name: "signed_url_azure_sas",
        input: "blob https://plaudstore.blob.core.windows.net/c/1.opus?sv=2021-08-06&sp=r&se=2026-01-01T00:00:00Z&sig=Ab12Cd34 queued",
        secret: "sig=Ab12Cd34",
        marker: "<redacted:signed_url host=plaudstore.blob.core.windows.net>",
    },
    Case {
        name: "signed_url_cloudfront_pair",
        input: "https://cdn.example.net/a.mp3?Expires=1799999999&Signature=ZZZ&Key-Pair-Id=APKAEX",
        secret: "Signature=ZZZ",
        marker: "<redacted:signed_url host=cdn.example.net>",
    },
    Case {
        name: "signed_url_json_escaped_slashes",
        input: r#"{"url":"https:\/\/plaud-media.s3.amazonaws.com\/u\/42.wav?X-Amz-Signature=4f1c9d2e"}"#,
        secret: "X-Amz-Signature",
        marker: "<redacted:signed_url host=plaud-media.s3.amazonaws.com>",
    },
    Case {
        name: "url_userinfo",
        input: "proxy configured as https://operator:hunter2@proxy.internal:8443/path",
        secret: "hunter2",
        marker: "<redacted:url_userinfo host=proxy.internal>",
    },
    Case {
        name: "device_token_16_hex",
        input: "device auth token 0123456789abcdef accepted",
        secret: "0123456789abcdef",
        marker: "<redacted:device_token>",
    },
    Case {
        name: "device_token_32_hex",
        input: "legacy static secret 0123456789abcdef0123456789abcdef",
        secret: "0123456789abcdef0123456789abcdef",
        marker: "<redacted:device_token>",
    },
    Case {
        name: "device_token_64_hex_uppercase",
        input: "handshake 0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF",
        secret: "0123456789ABCDEF0123456789ABCDEF",
        marker: "<redacted:device_token>",
    },
    Case {
        name: "device_token_split_across_debug_fields",
        input: "GattFrame { hi: \"0123456789abcdef\", lo: \"fedcba9876543210\" }",
        secret: "0123456789abcdef",
        marker: "<redacted:device_token>",
    },
    Case {
        name: "jwt",
        input: "session eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk stored",
        secret: "eyJhbGciOiJIUzI1NiJ9",
        marker: "<redacted:jwt>",
    },
    Case {
        name: "private_key_pem",
        input: "-----BEGIN EC PRIVATE KEY-----\nMHcCAQEEIBmL7Q2nnrs\nkQ8dYwF2hA==\n-----END EC PRIVATE KEY-----",
        secret: "MHcCAQEEIBmL7Q2nnrs",
        marker: "<redacted:private_key>",
    },
];

#[test]
fn every_shape_is_redacted() {
    let redactor = Redactor::new();
    for case in CASES {
        let output = redactor.redact(case.input);
        assert!(
            !output.contains(case.secret),
            "case `{}` leaked `{}`: {output}",
            case.name,
            case.secret
        );
        assert!(
            output.contains(case.marker),
            "case `{}` missing marker `{}`: {output}",
            case.name,
            case.marker
        );
    }
}

#[test]
fn signed_url_keeps_host_and_loses_everything_after_it() {
    let redactor = Redactor::new();
    let url = "https://plaud-media.s3.amazonaws.com/users/7/recordings/42.wav\
?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIAEXAMPLE%2F20260101%2Fus-east-1\
&X-Amz-Signature=4f1c9d2e8b7a61509f3c2b1a0d4e5f60";
    let line = format!("fetching {url} now");
    let output = redactor.redact(&line);
    assert_eq!(
        output.as_ref(),
        "fetching <redacted:signed_url host=plaud-media.s3.amazonaws.com> now"
    );
    for fragment in [
        "X-Amz-Signature",
        "X-Amz-Credential",
        "AKIAEXAMPLE",
        "/users/7/recordings/42.wav",
        "?",
    ] {
        assert!(
            !output.contains(fragment),
            "query fragment `{fragment}` survived: {output}"
        );
    }
}

#[test]
fn ordinary_urls_and_identifiers_survive() {
    let redactor = Redactor::new();
    for benign in [
        "GET https://openrouter.ai/api/v1/models?page=2 -> 200",
        "recording rec_7hd92kfa imported from 550e8400-e29b-41d4-a716-446655440000",
        "import finished: 3 recordings, 0 errors",
    ] {
        assert_eq!(
            redactor.redact(benign).as_ref(),
            benign,
            "benign line was altered"
        );
    }
}

#[test]
fn markers_stay_inside_the_documented_vocabulary() {
    let redactor = Redactor::new();
    for case in CASES {
        for finding in redactor.matches(case.input) {
            // Some renderers keep the field-name prefix (`api_key="…`) so the
            // record stays debuggable; the marker itself must still be one of
            // the documented ones.
            assert!(
                MARKER_PREFIXES
                    .iter()
                    .any(|prefix| finding.marker().contains(prefix)),
                "case `{}` rendered undocumented marker `{}`",
                case.name,
                finding.marker()
            );
        }
    }
}

#[test]
fn redaction_is_idempotent_over_the_whole_table() {
    let redactor = Redactor::new();
    redactor.register_secret("12345678").unwrap();
    let mut inputs: Vec<String> = CASES.iter().map(|case| case.input.to_owned()).collect();
    inputs.push("token=12345678;".to_owned());
    inputs.push(format!("key {OPENROUTER_KEY} and token=12345678"));
    for input in inputs {
        let once = redactor.redact(&input).into_owned();
        let twice = redactor.redact(&once).into_owned();
        let thrice = redactor.redact(&twice).into_owned();
        assert_eq!(once, twice, "redaction is not idempotent for `{input}`");
        assert_eq!(twice, thrice, "redaction is not stable for `{input}`");
    }
}
