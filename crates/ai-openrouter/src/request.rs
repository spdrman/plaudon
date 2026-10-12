//! The two narrowly typed request builders (spec 8.6).
//!
//! There are exactly two builders and no generic fallback. A model whose
//! family is `Unsupported` or `Unknown` has no builder at all, so an invalid
//! request for it is unrepresentable rather than merely discouraged.
//!
//! Neither builder holds a credential. The caller attaches authorization
//! through the egress gate, so a captured fixture or a log line can never
//! contain a key.

use plaudon_ai_contracts::capabilities::{MediaFormat, RequestFamily};
use plaudon_domain::locale::LanguageTag;
use serde::{Deserialize, Serialize};

use crate::{CHAT_COMPLETIONS_PATH, TRANSCRIPTIONS_PATH};

/// Why a request could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestBuildError {
    /// The model's request family has no builder.
    #[error("no request builder exists for this model's request family")]
    NoBuilderForFamily {
        /// The family that was asked for.
        family: RequestFamily,
    },
    /// The payload is larger than the verified limit for this model.
    #[error("the payload exceeds the verified limit for this model")]
    PayloadTooLarge {
        /// Verified limit in bytes.
        limit_bytes: u64,
        /// Size of the payload.
        actual_bytes: u64,
    },
    /// The media format is not in the model's verified accept list.
    #[error("the media format is not in this model's verified accept list")]
    FormatNotAccepted {
        /// The format that was offered.
        offered: MediaFormat,
    },
    /// The payload is empty.
    #[error("the payload is empty")]
    EmptyPayload,
}

/// A multipart speech-to-text request for `POST /audio/transcriptions`.
///
/// `audio` holds the exact bytes to upload; it is never serialized into the
/// JSON form of this type, so a captured fixture cannot contain audio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultipartSttRequest {
    /// Request path, relative to the API base.
    pub path: &'static str,
    /// Model to route to.
    pub model: String,
    /// File name presented in the multipart part. Derived from the container,
    /// never from the user's original filename.
    pub file_name: String,
    /// Content type of the audio part.
    pub content_type: String,
    /// Language hint, when the caller has one.
    pub language: Option<String>,
    /// Audio bytes. Excluded from every serialization.
    #[serde(skip)]
    pub audio: Vec<u8>,
}

impl MultipartSttRequest {
    /// Build a multipart request.
    ///
    /// Fails closed unless the model's family is `MultipartStt`, the format is
    /// in its verified accept list and the payload is inside its verified
    /// limit.
    pub fn build(
        family: RequestFamily,
        model: &str,
        format: &MediaFormat,
        accepted: &[MediaFormat],
        limit_bytes: Option<u64>,
        audio: Vec<u8>,
        language: Option<&LanguageTag>,
    ) -> Result<Self, RequestBuildError> {
        if family != RequestFamily::MultipartStt {
            return Err(RequestBuildError::NoBuilderForFamily { family });
        }
        validate_payload(format, accepted, limit_bytes, audio.len() as u64)?;
        Ok(Self {
            path: TRANSCRIPTIONS_PATH,
            model: model.to_owned(),
            file_name: format!("audio.{}", format.container),
            content_type: content_type_for(format),
            language: language.map(|tag| tag.as_str().to_owned()),
            audio,
        })
    }
}

/// A chat request carrying an audio part, for `POST /chat/completions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioChatRequest {
    /// Request path, relative to the API base.
    pub path: &'static str,
    /// Model to route to.
    pub model: String,
    /// Instruction sent alongside the audio.
    pub instruction: String,
    /// Container format name sent in the audio part.
    pub audio_format: String,
    /// Audio bytes. Excluded from every serialization.
    #[serde(skip)]
    pub audio: Vec<u8>,
}

impl AudioChatRequest {
    /// Build a chat request carrying audio.
    pub fn build(
        family: RequestFamily,
        model: &str,
        instruction: &str,
        format: &MediaFormat,
        accepted: &[MediaFormat],
        limit_bytes: Option<u64>,
        audio: Vec<u8>,
    ) -> Result<Self, RequestBuildError> {
        if family != RequestFamily::AudioChat {
            return Err(RequestBuildError::NoBuilderForFamily { family });
        }
        validate_payload(format, accepted, limit_bytes, audio.len() as u64)?;
        Ok(Self {
            path: CHAT_COMPLETIONS_PATH,
            model: model.to_owned(),
            instruction: instruction.to_owned(),
            audio_format: format.container.clone(),
            audio,
        })
    }
}

fn validate_payload(
    format: &MediaFormat,
    accepted: &[MediaFormat],
    limit_bytes: Option<u64>,
    actual_bytes: u64,
) -> Result<(), RequestBuildError> {
    if actual_bytes == 0 {
        return Err(RequestBuildError::EmptyPayload);
    }
    if !accepted.contains(format) {
        return Err(RequestBuildError::FormatNotAccepted {
            offered: format.clone(),
        });
    }
    if let Some(limit) = limit_bytes {
        if actual_bytes > limit {
            return Err(RequestBuildError::PayloadTooLarge {
                limit_bytes: limit,
                actual_bytes,
            });
        }
    }
    Ok(())
}

fn content_type_for(format: &MediaFormat) -> String {
    match format.container.as_str() {
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "m4a" | "mp4" => "audio/mp4",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        _ => "application/octet-stream",
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav() -> MediaFormat {
        MediaFormat::new("wav", "pcm_s16le")
    }

    #[test]
    fn an_unknown_or_unsupported_family_has_no_builder() {
        for family in [RequestFamily::Unknown, RequestFamily::Unsupported] {
            assert_eq!(
                MultipartSttRequest::build(family, "m", &wav(), &[wav()], None, vec![1], None),
                Err(RequestBuildError::NoBuilderForFamily { family })
            );
            assert_eq!(
                AudioChatRequest::build(family, "m", "go", &wav(), &[wav()], None, vec![1]),
                Err(RequestBuildError::NoBuilderForFamily { family })
            );
        }
    }

    #[test]
    fn a_chat_family_model_cannot_be_sent_to_the_transcription_endpoint() {
        assert_eq!(
            MultipartSttRequest::build(
                RequestFamily::AudioChat,
                "m",
                &wav(),
                &[wav()],
                None,
                vec![1],
                None
            ),
            Err(RequestBuildError::NoBuilderForFamily {
                family: RequestFamily::AudioChat
            })
        );
    }

    #[test]
    fn a_multipart_model_cannot_be_sent_a_chat_payload() {
        assert_eq!(
            AudioChatRequest::build(
                RequestFamily::MultipartStt,
                "m",
                "go",
                &wav(),
                &[wav()],
                None,
                vec![1]
            ),
            Err(RequestBuildError::NoBuilderForFamily {
                family: RequestFamily::MultipartStt
            })
        );
    }

    #[test]
    fn a_valid_multipart_request_targets_the_dedicated_path_and_hides_the_audio() {
        let request = MultipartSttRequest::build(
            RequestFamily::MultipartStt,
            "vendor/stt",
            &wav(),
            &[wav()],
            Some(1_024),
            b"RIFFfake".to_vec(),
            Some(&LanguageTag::parse("en-US").unwrap()),
        )
        .unwrap();

        assert_eq!(request.path, "/audio/transcriptions");
        assert_eq!(request.file_name, "audio.wav");
        assert_eq!(request.content_type, "audio/wav");
        assert_eq!(request.language.as_deref(), Some("en-US"));

        let json = serde_json::to_string(&request).unwrap();
        assert!(
            !json.contains("RIFF"),
            "audio bytes must never be serialized: {json}"
        );
        assert!(
            !json.contains("audio\":["),
            "audio bytes must never be serialized: {json}"
        );
    }

    #[test]
    fn declared_limits_and_accept_lists_are_enforced() {
        assert_eq!(
            MultipartSttRequest::build(
                RequestFamily::MultipartStt,
                "m",
                &wav(),
                &[wav()],
                Some(4),
                vec![0; 5],
                None
            ),
            Err(RequestBuildError::PayloadTooLarge {
                limit_bytes: 4,
                actual_bytes: 5
            })
        );
        let opus = MediaFormat::new("ogg", "opus");
        assert_eq!(
            MultipartSttRequest::build(
                RequestFamily::MultipartStt,
                "m",
                &opus,
                &[wav()],
                None,
                vec![1],
                None
            ),
            Err(RequestBuildError::FormatNotAccepted { offered: opus })
        );
        assert_eq!(
            MultipartSttRequest::build(
                RequestFamily::MultipartStt,
                "m",
                &wav(),
                &[wav()],
                None,
                Vec::new(),
                None
            ),
            Err(RequestBuildError::EmptyPayload)
        );
    }

    #[test]
    fn the_uploaded_file_name_is_derived_from_the_container_not_the_user_filename() {
        let request = MultipartSttRequest::build(
            RequestFamily::MultipartStt,
            "m",
            &MediaFormat::new("mp3", "mp3"),
            &[MediaFormat::new("mp3", "mp3")],
            None,
            vec![1],
            None,
        )
        .unwrap();
        assert_eq!(request.file_name, "audio.mp3");
        assert_eq!(request.content_type, "audio/mpeg");
    }
}
