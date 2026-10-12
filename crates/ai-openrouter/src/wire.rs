//! Wire encoding and decoding for the two audio request families and for text
//! generation (spec 8.6, 9.5, 10.3).
//!
//! Encoding and decoding are kept in one module so the request shape and the
//! response shape that was actually observed for it stay reviewable side by
//! side. Nothing here holds a credential: the `Authorization` header is
//! attached by the client immediately before dispatch and never enters an
//! encoded body or a captured fixture.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::request::{AudioChatRequest, MultipartSttRequest, TextGenerationRequest};

/// A body ready to hand to the egress gate.
#[derive(Clone, PartialEq, Eq)]
pub struct EncodedBody {
    /// Value for the `Content-Type` header.
    pub content_type: String,
    /// Encoded bytes.
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for EncodedBody {
    /// Renders the length only: the body may contain the user's audio or
    /// transcript (spec 15.4).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncodedBody")
            .field("content_type", &self.content_type)
            .field("bytes", &format_args!("<{} bytes>", self.bytes.len()))
            .finish()
    }
}

/// Multipart boundary. Fixed-prefix plus caller-supplied nonce so a request is
/// reproducible in a fixture while still being unambiguous.
#[must_use]
pub fn multipart_boundary(nonce: &str) -> String {
    format!("----plaudon{nonce}")
}

impl MultipartSttRequest {
    /// Encode as `multipart/form-data` for `POST /audio/transcriptions`.
    ///
    /// Part order is fixed (`model`, `response_format`, optional `language`,
    /// then `file`) so a captured fixture is byte-comparable.
    #[must_use]
    pub fn encode(&self, boundary: &str) -> EncodedBody {
        let mut bytes = Vec::with_capacity(self.audio.len() + 512);
        push_field(&mut bytes, boundary, "model", &self.model);
        push_field(&mut bytes, boundary, "response_format", "verbose_json");
        if let Some(language) = &self.language {
            push_field(&mut bytes, boundary, "language", language);
        }
        bytes.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        bytes.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n",
                self.file_name
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(format!("Content-Type: {}\r\n\r\n", self.content_type).as_bytes());
        bytes.extend_from_slice(&self.audio);
        bytes.extend_from_slice(b"\r\n");
        bytes.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        EncodedBody {
            content_type: format!("multipart/form-data; boundary={boundary}"),
            bytes,
        }
    }
}

fn push_field(out: &mut Vec<u8>, boundary: &str, name: &str, value: &str) {
    out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    out.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
    );
    out.extend_from_slice(value.as_bytes());
    out.extend_from_slice(b"\r\n");
}

impl AudioChatRequest {
    /// Encode as a chat completion carrying an `input_audio` part.
    #[must_use]
    pub fn encode(&self) -> EncodedBody {
        let body = serde_json::json!({
            "model": self.model,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": self.instruction },
                    {
                        "type": "input_audio",
                        "input_audio": {
                            "data": base64_encode(&self.audio),
                            "format": self.audio_format,
                        }
                    }
                ]
            }],
        });
        EncodedBody {
            content_type: "application/json".to_owned(),
            bytes: serde_json::to_vec(&body).unwrap_or_default(),
        }
    }
}

impl TextGenerationRequest {
    /// Encode as a chat completion, with JSON-schema response format when the
    /// model was observed to support it (spec 9.5).
    #[must_use]
    pub fn encode(&self) -> EncodedBody {
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": self.system_instruction },
                { "role": "user", "content": self.user_content },
            ],
        });
        if let Some(max_tokens) = self.max_output_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if self.structured_output {
            body["response_format"] = serde_json::json!({
                "type": "json_schema",
                "json_schema": {
                    "name": self.schema_name,
                    "strict": true,
                    "schema": self.output_schema,
                }
            });
        }
        EncodedBody {
            content_type: "application/json".to_owned(),
            bytes: serde_json::to_vec(&body).unwrap_or_default(),
        }
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
///
/// Hand-rolled rather than adding a dependency for forty lines; exercised by a
/// round-trip property test against the published RFC 4648 vectors.
#[must_use]
pub fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[((triple >> 18) & 0x3f) as usize] as char);
        out.push(B64[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

/// Usage block as returned by the provider.
///
/// Every field is optional because the provider is not obliged to report any of
/// them, and an absent field is `None`, never `0` (spec 10.4 "unknown is not
/// zero").
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageDto {
    /// Prompt tokens charged.
    #[serde(default)]
    pub prompt_tokens: Option<u64>,
    /// Completion tokens charged.
    #[serde(default)]
    pub completion_tokens: Option<u64>,
    /// Total tokens charged.
    #[serde(default)]
    pub total_tokens: Option<u64>,
    /// Seconds of audio charged, when the endpoint reports it.
    #[serde(default)]
    pub input_audio_seconds: Option<f64>,
    /// Final charge in USD credits, when the provider returns one.
    #[serde(default)]
    pub cost: Option<f64>,
    /// Anything else the provider reported, preserved verbatim.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

/// One segment of a transcription response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SegmentDto {
    /// Provider-assigned index, when present.
    #[serde(default)]
    pub id: Option<i64>,
    /// Start offset in seconds.
    #[serde(default)]
    pub start: Option<f64>,
    /// End offset in seconds.
    #[serde(default)]
    pub end: Option<f64>,
    /// Segment text.
    #[serde(default)]
    pub text: String,
    /// Provider confidence, on the provider's own scale.
    #[serde(default)]
    pub confidence: Option<f64>,
    /// Speaker label, when the endpoint diarizes.
    #[serde(default)]
    pub speaker: Option<String>,
    /// Anything else the provider reported.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

/// One word alignment of a transcription response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WordDto {
    /// Word text.
    #[serde(default)]
    pub word: String,
    /// Start offset in seconds.
    #[serde(default)]
    pub start: Option<f64>,
    /// End offset in seconds.
    #[serde(default)]
    pub end: Option<f64>,
    /// Anything else the provider reported.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

/// Decoded `POST /audio/transcriptions` response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TranscriptionResponse {
    /// Full text. The one field the endpoint always returns.
    pub text: String,
    /// Detected language, when the endpoint reports one.
    #[serde(default)]
    pub language: Option<String>,
    /// Media duration in seconds, when the endpoint reports one.
    #[serde(default)]
    pub duration: Option<f64>,
    /// Segment offsets, when the endpoint reports them.
    #[serde(default)]
    pub segments: Vec<SegmentDto>,
    /// Word offsets, when the endpoint reports them.
    #[serde(default)]
    pub words: Vec<WordDto>,
    /// Usage block, when the endpoint reports one.
    #[serde(default)]
    pub usage: Option<UsageDto>,
    /// Anything else the provider reported, preserved rather than dropped.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

impl TranscriptionResponse {
    /// Whether the response carried any real source-time offset.
    ///
    /// Spec 8.6A: a text-only result is valid, but it must not be presented
    /// with invented seek times.
    #[must_use]
    pub fn has_native_offsets(&self) -> bool {
        self.segments
            .iter()
            .any(|segment| segment.start.is_some() && segment.end.is_some())
            || self
                .words
                .iter()
                .any(|word| word.start.is_some() && word.end.is_some())
    }

    /// Whether word-level offsets were observed.
    #[must_use]
    pub fn has_word_offsets(&self) -> bool {
        self.words
            .iter()
            .any(|word| word.start.is_some() && word.end.is_some())
    }

    /// Whether any segment carried a speaker label.
    #[must_use]
    pub fn has_speaker_labels(&self) -> bool {
        self.segments.iter().any(|s| s.speaker.is_some())
    }
}

/// One choice of a chat completion response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChoiceDto {
    /// Assistant message.
    #[serde(default)]
    pub message: MessageDto,
    /// Why generation stopped, when reported.
    #[serde(default)]
    pub finish_reason: Option<String>,
    /// Anything else the provider reported.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

/// Assistant message of a chat completion response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MessageDto {
    /// Message role.
    #[serde(default)]
    pub role: String,
    /// Message content.
    #[serde(default)]
    pub content: Option<String>,
    /// Anything else the provider reported.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

/// Decoded `POST /chat/completions` response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    /// Provider-assigned response identifier, preserved as the vendor request
    /// id when no header carries one.
    #[serde(default)]
    pub id: Option<String>,
    /// Model actually served, which may differ from the one requested.
    #[serde(default)]
    pub model: Option<String>,
    /// Downstream provider name, when OpenRouter discloses it.
    #[serde(default)]
    pub provider: Option<String>,
    /// Choices. The endpoint may return zero.
    #[serde(default)]
    pub choices: Vec<ChoiceDto>,
    /// Usage block, when reported.
    #[serde(default)]
    pub usage: Option<UsageDto>,
    /// Anything else the provider reported.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

impl ChatCompletionResponse {
    /// Text of the first choice, when there is one.
    #[must_use]
    pub fn first_content(&self) -> Option<&str> {
        self.choices
            .first()
            .and_then(|choice| choice.message.content.as_deref())
    }
}

/// The provider's error envelope.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderErrorBody {
    /// Error block.
    #[serde(default)]
    pub error: Option<ProviderErrorDetail>,
}

/// Detail of a provider error. Only the numeric code is used for routing; the
/// message is never shown verbatim to the user (spec 15.4 fixed vocabulary).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderErrorDetail {
    /// Provider error code.
    #[serde(default)]
    pub code: Option<serde_json::Value>,
    /// Provider message. Diagnostic only.
    #[serde(default)]
    pub message: Option<String>,
    /// Anything else the provider reported.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

/// Header carrying the vendor request id, when the provider sets one.
pub const REQUEST_ID_HEADERS: [&str; 2] = ["x-request-id", "openrouter-request-id"];

/// Extract the vendor request id from response headers.
#[must_use]
pub fn vendor_request_id(headers: &BTreeMap<String, String>) -> Option<String> {
    REQUEST_ID_HEADERS
        .iter()
        .find_map(|name| headers.get(*name).cloned())
}

/// Parse `Retry-After`, accepting both the delay-seconds and the HTTP-date
/// forms (spec 10.6).
///
/// `now_unix` is supplied rather than read from the clock so the parse is
/// testable and deterministic.
#[must_use]
pub fn parse_retry_after(raw: &str, now_unix: i64) -> Option<u32> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(seconds) = trimmed.parse::<i64>() {
        return u32::try_from(seconds.max(0)).ok();
    }
    let epoch = parse_http_date(trimmed)?;
    u32::try_from((epoch - now_unix).max(0)).ok()
}

/// Parse an IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`) into epoch seconds.
///
/// Only the one format RFC 9110 requires senders to produce is accepted;
/// anything else is unknown rather than guessed.
fn parse_http_date(raw: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let rest = raw.split_once(", ")?.1;
    let mut parts = rest.split(' ');
    let day: i64 = parts.next()?.parse().ok()?;
    let month_name = parts.next()?;
    let month = MONTHS.iter().position(|m| *m == month_name)? as i64 + 1;
    let year: i64 = parts.next()?.parse().ok()?;
    let time = parts.next()?;
    if parts.next()? != "GMT" {
        return None;
    }
    let mut hms = time.split(':');
    let hour: i64 = hms.next()?.parse().ok()?;
    let minute: i64 = hms.next()?.parse().ok()?;
    let second: i64 = hms.next()?.parse().ok()?;
    if !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use plaudon_ai_contracts::capabilities::{MediaFormat, RequestFamily};

    fn wav() -> MediaFormat {
        MediaFormat::new("wav", "pcm_s16le")
    }

    #[test]
    fn base64_matches_the_rfc_4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(&[0xff, 0xfe, 0xfd]), "//79");
    }

    #[test]
    fn the_multipart_body_carries_the_audio_exactly_once() {
        let request = MultipartSttRequest::build(
            RequestFamily::MultipartStt,
            "openai/whisper-1",
            &wav(),
            &[wav()],
            None,
            b"AUDIOBYTES".to_vec(),
            None,
        )
        .expect("builds");
        let encoded = request.encode(&multipart_boundary("test"));
        let text = String::from_utf8_lossy(&encoded.bytes);
        assert!(encoded
            .content_type
            .starts_with("multipart/form-data; boundary="));
        assert_eq!(text.matches("AUDIOBYTES").count(), 1);
        assert!(text.contains("name=\"model\""));
        assert!(text.contains("openai/whisper-1"));
        assert!(text.contains("filename=\"audio.wav\""));
        assert!(text.ends_with("------plaudontest--\r\n"));
    }

    #[test]
    fn an_encoded_body_never_renders_its_bytes() {
        let encoded = EncodedBody {
            content_type: "application/json".to_owned(),
            bytes: b"{\"secret\":\"transcript\"}".to_vec(),
        };
        let rendered = format!("{encoded:?}");
        assert!(!rendered.contains("transcript"), "{rendered}");
        assert!(rendered.contains("<23 bytes>"));
    }

    #[test]
    fn a_transcription_response_decodes_without_inventing_offsets() {
        let decoded: TranscriptionResponse =
            serde_json::from_str(r#"{"text":"hello","extra_field":{"a":1}}"#).expect("decodes");
        assert_eq!(decoded.text, "hello");
        assert!(decoded.segments.is_empty());
        assert!(!decoded.has_native_offsets());
        assert!(decoded.unknown.contains_key("extra_field"));
    }

    #[test]
    fn segments_without_both_bounds_are_not_native_offsets() {
        let decoded: TranscriptionResponse =
            serde_json::from_str(r#"{"text":"hi","segments":[{"text":"hi","start":0.0}]}"#)
                .expect("decodes");
        assert!(!decoded.has_native_offsets());
    }

    #[test]
    fn retry_after_accepts_seconds_and_http_dates() {
        assert_eq!(parse_retry_after("30", 0), Some(30));
        assert_eq!(parse_retry_after(" 0 ", 0), Some(0));
        assert_eq!(parse_retry_after("-5", 0), Some(0));
        // 1994-11-06T08:49:37Z is 784111777.
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", 784_111_717),
            Some(60)
        );
        // A date already in the past means "retry now", not a negative wait.
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", 784_111_900),
            Some(0)
        );
    }

    #[test]
    fn a_malformed_retry_after_is_unknown_rather_than_guessed() {
        for raw in ["", "soon", "Sun 06 Nov 1994 08:49:37 GMT", "1994-11-06"] {
            assert_eq!(parse_retry_after(raw, 0), None, "{raw}");
        }
    }

    #[test]
    fn the_vendor_request_id_comes_from_either_documented_header() {
        let mut headers = BTreeMap::new();
        headers.insert("x-request-id".to_owned(), "abc".to_owned());
        assert_eq!(vendor_request_id(&headers), Some("abc".to_owned()));
        assert_eq!(vendor_request_id(&BTreeMap::new()), None);
    }
}
