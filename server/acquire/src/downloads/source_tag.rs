//! Which download source a task, attempt or handle belongs to.
//!
//! The journal, the stored handles and the source-priority setting keep a
//! source as text (`soulseek`, `usenet`, `plugin:<name>`). This module is
//! the one place that writes and reads that text; everything else compares
//! [`SourceTag`] values.

use std::fmt;
use std::str::FromStr;

/// Stored text of the Soulseek source.
const SOULSEEK: &str = "soulseek";
/// Stored text of the Usenet source.
const USENET: &str = "usenet";
/// Prefix of a plugin source's stored text.
const PLUGIN_PREFIX: &str = "plugin:";

/// A download source.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SourceTag {
    /// Soulseek through slskd.
    Soulseek,
    /// Usenet through SABnzbd.
    Usenet,
    /// A plugin's download client, by plugin name.
    Plugin(String),
}

/// Stored text that names no source.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown download source {0:?}")]
pub struct UnknownSource(pub String);

impl SourceTag {
    /// The plugin source for a plugin host key (`plugin:<name>`), or for
    /// a bare plugin name.
    pub fn plugin(key: &str) -> Self {
        Self::Plugin(key.strip_prefix(PLUGIN_PREFIX).unwrap_or(key).to_owned())
    }

    /// The source as people know it: "Soulseek", "Usenet", or the
    /// plugin's name.
    pub fn label(&self) -> &str {
        match self {
            Self::Soulseek => "Soulseek",
            Self::Usenet => "Usenet",
            Self::Plugin(name) => name,
        }
    }

    /// The download client behind the source: `slskd`, `sabnzbd`, or the
    /// plugin's name.
    pub fn client(&self) -> &str {
        match self {
            Self::Soulseek => "slskd",
            Self::Usenet => "sabnzbd",
            Self::Plugin(name) => name,
        }
    }
}

impl fmt::Display for SourceTag {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Soulseek => out.write_str(SOULSEEK),
            Self::Usenet => out.write_str(USENET),
            Self::Plugin(name) => write!(out, "{PLUGIN_PREFIX}{name}"),
        }
    }
}

impl FromStr for SourceTag {
    type Err = UnknownSource;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            SOULSEEK => Ok(Self::Soulseek),
            USENET => Ok(Self::Usenet),
            other => match other.strip_prefix(PLUGIN_PREFIX) {
                Some(name) if !name.is_empty() => Ok(Self::Plugin(name.to_owned())),
                _ => Err(UnknownSource(other.to_owned())),
            },
        }
    }
}

impl PartialEq<str> for SourceTag {
    fn eq(&self, text: &str) -> bool {
        match self {
            Self::Soulseek => text == SOULSEEK,
            Self::Usenet => text == USENET,
            Self::Plugin(name) => text
                .strip_prefix(PLUGIN_PREFIX)
                .is_some_and(|stored| stored == name),
        }
    }
}

impl PartialEq<&str> for SourceTag {
    fn eq(&self, text: &&str) -> bool {
        self == *text
    }
}

impl PartialEq<String> for SourceTag {
    fn eq(&self, text: &String) -> bool {
        self == text.as_str()
    }
}

impl serde::Serialize for SourceTag {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for SourceTag {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

impl rusqlite::types::ToSql for SourceTag {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::from(self.to_string()))
    }
}

impl rusqlite::types::FromSql for SourceTag {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let text = value.as_str()?;
        text.parse()
            .map_err(|error| rusqlite::types::FromSqlError::Other(Box::new(error)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_text_round_trips() {
        for (tag, text) in [
            (SourceTag::Soulseek, "soulseek"),
            (SourceTag::Usenet, "usenet"),
            (SourceTag::Plugin("toy".to_owned()), "plugin:toy"),
        ] {
            assert_eq!(tag.to_string(), text);
            assert_eq!(text.parse::<SourceTag>(), Ok(tag.clone()));
            assert_eq!(tag, *text);
            let json = serde_json::to_string(&tag).unwrap();
            assert_eq!(json, format!("\"{text}\""));
            assert_eq!(serde_json::from_str::<SourceTag>(&json).unwrap(), tag);
        }
        for unknown in ["", "slskd", "plugin:", "Soulseek"] {
            assert!(unknown.parse::<SourceTag>().is_err(), "{unknown}");
        }
        assert_eq!(SourceTag::plugin("plugin:toy"), SourceTag::plugin("toy"));
    }
}
