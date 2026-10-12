//! Property: a registered live secret never survives redaction, wherever it
//! sits in the record (spec §1.5 "0 secrets in application logs").

#![allow(clippy::unwrap_used, clippy::panic)]

use plaudon_diagnostics::Redactor;
use proptest::prelude::*;

/// The shapes a secret can occupy in a real record.
fn embeddings(secret: &str) -> Vec<String> {
    #[derive(Debug)]
    #[allow(dead_code)]
    struct DeviceSession {
        device: &'static str,
        token: String,
        attempts: u32,
    }

    let debug_struct = DeviceSession {
        device: "PLAUD Note",
        token: secret.to_owned(),
        attempts: 2,
    };

    vec![
        // start of the message
        format!("{secret} was accepted by the device"),
        // middle of the message
        format!("device accepted {secret} after one retry"),
        // end of the message
        format!("device accepted credential {secret}"),
        // inside a field value
        format!("event=auth outcome=ok credential=\"{secret}\""),
        // inside JSON
        format!("{{\"event\":\"auth\",\"credential\":\"{secret}\"}}"),
        // inside Debug output
        format!("{debug_struct:?}"),
        // split across two adjacent fields
        split_across_fields(secret),
    ]
}

fn split_across_fields(secret: &str) -> String {
    let mid = (1..secret.len())
        .find(|i| secret.is_char_boundary(*i))
        .unwrap_or(0);
    let (head, tail) = secret.split_at(mid);
    format!("head=\"{head}\" tail=\"{tail}\"")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn registered_secret_never_survives(
        secret in "[A-Za-z0-9_\\-]{8,64}",
    ) {
        let redactor = Redactor::new();
        redactor.register_secret(&secret).unwrap();
        for record in embeddings(&secret) {
            let output = redactor.redact(&record);
            prop_assert!(
                !output.contains(&secret),
                "secret survived in `{record}` -> `{output}`"
            );
        }
    }

    #[test]
    fn registered_secret_with_punctuation_never_survives(
        secret in "[A-Za-z0-9/+=:.@!#$%^&*()\\[\\]{}|\\\\ ]{12,48}",
    ) {
        let redactor = Redactor::new();
        prop_assume!(redactor.register_secret(&secret).is_ok());
        for record in embeddings(&secret) {
            let output = redactor.redact(&record);
            prop_assert!(
                !output.contains(&secret),
                "secret survived in `{record}` -> `{output}`"
            );
        }
    }

    #[test]
    fn redaction_is_idempotent_for_registered_secrets(
        secret in "[A-Za-z0-9_\\-]{8,64}",
    ) {
        let redactor = Redactor::new();
        redactor.register_secret(&secret).unwrap();
        for record in embeddings(&secret) {
            let once = redactor.redact(&record).into_owned();
            let twice = redactor.redact(&once).into_owned();
            prop_assert_eq!(once, twice);
        }
    }

    #[test]
    fn unregistered_shapeless_text_is_left_alone(
        text in "[a-z ]{1,40}",
    ) {
        let redactor = Redactor::new();
        prop_assert_eq!(redactor.redact(&text).into_owned(), text);
    }
}
