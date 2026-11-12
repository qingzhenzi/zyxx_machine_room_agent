//! Small value types shared by the spec model.

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};

/// A byte quantity rendered with Docker-style suffixes (`512m`, `1g`, `1024k`).
///
/// Docker's API consumes bytes, but humans write sizes. This type keeps the
/// conversion in one place and serializes back to a compact string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteSize(pub u64);

impl ByteSize {
    /// Bytes.
    pub const fn bytes(value: u64) -> Self {
        Self(value)
    }

    /// KiB (1024 bytes).
    pub const fn kib(value: u64) -> Self {
        Self(value * 1024)
    }

    /// MiB (1024 KiB).
    pub const fn mib(value: u64) -> Self {
        Self(value * 1024 * 1024)
    }

    /// GiB (1024 MiB).
    pub const fn gib(value: u64) -> Self {
        Self(value * 1024 * 1024 * 1024)
    }

    /// Raw bytes.
    pub const fn as_bytes(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.0;
        if value >= 1024 * 1024 * 1024 && value % (1024 * 1024 * 1024) == 0 {
            write!(f, "{}g", value / (1024 * 1024 * 1024))
        } else if value >= 1024 * 1024 && value % (1024 * 1024) == 0 {
            write!(f, "{}m", value / (1024 * 1024))
        } else if value >= 1024 && value % 1024 == 0 {
            write!(f, "{}k", value / 1024)
        } else {
            write!(f, "{}b", value)
        }
    }
}

impl FromStr for ByteSize {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        let text = raw.trim().to_ascii_lowercase();
        if text.is_empty() {
            return Err("empty size".into());
        }

        let split = text
            .find(|ch: char| !ch.is_ascii_digit())
            .unwrap_or(text.len());
        let (number, suffix) = text.split_at(split);
        let value: u64 = number
            .parse()
            .map_err(|_| format!("invalid number in size `{raw}`"))?;

        let multiplier = match suffix.trim() {
            "" | "b" | "byte" | "bytes" => 1,
            "k" | "kb" | "kib" => 1024,
            "m" | "mb" | "mib" => 1024 * 1024,
            "g" | "gb" | "gib" => 1024 * 1024 * 1024,
            other => return Err(format!("unknown size suffix `{other}` in `{raw}`")),
        };

        value
            .checked_mul(multiplier)
            .map(Self)
            .ok_or_else(|| format!("size `{raw}` overflows u64"))
    }
}

impl Serialize for ByteSize {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct ByteSizeVisitor;

        impl Visitor<'_> for ByteSizeVisitor {
            type Value = ByteSize;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a byte size such as `512m` or an integer number of bytes")
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> std::result::Result<Self::Value, E> {
                Ok(ByteSize(value))
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> std::result::Result<Self::Value, E> {
                u64::try_from(value)
                    .map(ByteSize)
                    .map_err(|_| E::custom("byte size must be non-negative"))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Self::Value, E> {
                value.parse().map_err(E::custom)
            }

            fn visit_string<E: de::Error>(
                self,
                value: String,
            ) -> std::result::Result<Self::Value, E> {
                self.visit_str(&value)
            }
        }

        deserializer.deserialize_any(ByteSizeVisitor)
    }
}

/// Validate a Docker object name (`[a-zA-Z0-9][a-zA-Z0-9_.-]*`).
pub fn validate_name(kind: &str, name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::InvalidSpec(format!("{kind} name must not be empty")));
    }
    if name.len() > 128 {
        return Err(Error::InvalidSpec(format!(
            "{kind} name `{name}` is longer than 128 characters"
        )));
    }

    let mut chars = name.chars();
    let first = chars.next().expect("checked non-empty");
    if !first.is_ascii_alphanumeric() {
        return Err(Error::InvalidSpec(format!(
            "{kind} name `{name}` must start with an ASCII letter or digit"
        )));
    }
    if let Some(bad) =
        chars.find(|ch| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-')))
    {
        return Err(Error::InvalidSpec(format!(
            "{kind} name `{name}` contains unsupported character `{bad}`"
        )));
    }
    Ok(())
}

/// Validate a Linux bridge interface name (IFNAMSIZ is 16 bytes including NUL).
pub fn validate_bridge_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::InvalidSpec(
            "bridge_name must not be empty".to_string(),
        ));
    }
    if name.len() > 15 {
        return Err(Error::InvalidSpec(format!(
            "bridge_name `{name}` is longer than 15 bytes (Linux interface limit)"
        )));
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
    {
        return Err(Error::InvalidSpec(format!(
            "bridge_name `{name}` contains unsupported characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes() {
        assert_eq!("512m".parse::<ByteSize>().unwrap(), ByteSize::mib(512));
        assert_eq!("1g".parse::<ByteSize>().unwrap(), ByteSize::gib(1));
        assert_eq!("1024k".parse::<ByteSize>().unwrap(), ByteSize::mib(1));
        assert_eq!("42".parse::<ByteSize>().unwrap(), ByteSize::bytes(42));
        assert!("12x".parse::<ByteSize>().is_err());
    }

    #[test]
    fn renders_sizes() {
        assert_eq!(ByteSize::mib(512).to_string(), "512m");
        assert_eq!(ByteSize::gib(2).to_string(), "2g");
        assert_eq!(ByteSize::bytes(1000).to_string(), "1000b");
    }

    #[test]
    fn validates_names() {
        assert!(validate_name("container", "py-api_1.2").is_ok());
        assert!(validate_name("container", "bad name").is_err());
        assert!(validate_name("container", "-bad").is_err());
        assert!(validate_bridge_name("docker1").is_ok());
        assert!(validate_bridge_name("this-name-is-way-too-long").is_err());
    }
}
