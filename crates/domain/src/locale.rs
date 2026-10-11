//! BCP 47 language tags.
//!
//! Spec 6.5 requires locale to be a type rather than a bare string so that an
//! engine cannot claim support for an unparsed value.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Reason a language tag was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LanguageTagError {
    /// The tag was empty.
    #[error("language tag is empty")]
    Empty,
    /// The tag contained a character outside the BCP 47 subset we accept.
    #[error("language tag contains an unsupported character")]
    InvalidCharacter,
    /// A subtag had an unusable length.
    #[error("language tag subtag has an invalid length")]
    InvalidSubtagLength,
}

/// A normalized BCP 47 language tag such as `en-US`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct LanguageTag(String);

impl LanguageTag {
    /// Parse and normalize a tag: language lowercase, region uppercase.
    pub fn parse(raw: &str) -> Result<Self, LanguageTagError> {
        if raw.is_empty() {
            return Err(LanguageTagError::Empty);
        }
        let mut parts = Vec::new();
        for (index, subtag) in raw.split('-').enumerate() {
            if subtag.is_empty() || subtag.len() > 8 {
                return Err(LanguageTagError::InvalidSubtagLength);
            }
            if !subtag.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Err(LanguageTagError::InvalidCharacter);
            }
            if index == 0 {
                if subtag.len() < 2 || !subtag.chars().all(|c| c.is_ascii_alphabetic()) {
                    return Err(LanguageTagError::InvalidSubtagLength);
                }
                parts.push(subtag.to_ascii_lowercase());
            } else if subtag.len() == 2 && subtag.chars().all(|c| c.is_ascii_alphabetic()) {
                parts.push(subtag.to_ascii_uppercase());
            } else {
                parts.push(subtag.to_ascii_lowercase());
            }
        }
        Ok(Self(parts.join("-")))
    }

    /// Primary language subtag.
    #[must_use]
    pub fn primary_language(&self) -> &str {
        self.0.split('-').next().unwrap_or(&self.0)
    }

    /// The normalized tag.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LanguageTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for LanguageTag {
    type Err = LanguageTagError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl<'de> Deserialize<'de> for LanguageTag {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_normalized() {
        assert_eq!(LanguageTag::parse("EN-us").unwrap().as_str(), "en-US");
        assert_eq!(LanguageTag::parse("pt-br").unwrap().as_str(), "pt-BR");
        assert_eq!(LanguageTag::parse("de").unwrap().primary_language(), "de");
    }

    #[test]
    fn malformed_tags_are_rejected() {
        assert_eq!(LanguageTag::parse(""), Err(LanguageTagError::Empty));
        assert_eq!(LanguageTag::parse("e"), Err(LanguageTagError::InvalidSubtagLength));
        assert_eq!(LanguageTag::parse("en-"), Err(LanguageTagError::InvalidSubtagLength));
        assert_eq!(LanguageTag::parse("en_US"), Err(LanguageTagError::InvalidCharacter));
    }

    #[test]
    fn deserialization_rejects_malformed_tags() {
        assert!(serde_json::from_str::<LanguageTag>("\"en_US\"").is_err());
    }
}
