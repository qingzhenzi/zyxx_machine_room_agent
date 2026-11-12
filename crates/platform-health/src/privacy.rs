//! Privacy controls for collected data.
//!
//! Device inventory is personal data in most jurisdictions: hostnames, MAC
//! addresses and hardware serials identify a person's machine. The default is
//! [`Privacy::Full`] because the crate is normally used inside a managed fleet,
//! but anything leaving that boundary (telemetry, support bundles) should use
//! [`Privacy::Redacted`].

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Privacy {
    /// Report values verbatim.
    #[default]
    Full,
    /// Mask MAC addresses, serials and hostnames.
    Redacted,
}

impl Privacy {
    pub fn is_redacted(self) -> bool {
        matches!(self, Privacy::Redacted)
    }

    /// Mask the device-specific half of a MAC address, keeping the vendor OUI
    /// (which is not identifying and is useful for inventory).
    ///
    /// `aa:bb:cc:dd:ee:ff` -> `aa:bb:cc:**:**:**`
    pub fn mac(self, raw: &str) -> String {
        if !self.is_redacted() || raw.is_empty() {
            return raw.to_string();
        }
        let separator = if raw.contains('-') { '-' } else { ':' };
        let parts: Vec<&str> = raw.split(separator).collect();
        if parts.len() < 4 {
            return "**".to_string();
        }
        let mut out: Vec<&str> = parts[..3].to_vec();
        out.resize(parts.len(), "**");
        out.join(&separator.to_string())
    }

    /// Mask all but the last four characters of a serial number or asset tag.
    ///
    /// `W1KS427111E` -> `*******111E`
    pub fn secret(self, raw: &str) -> String {
        if !self.is_redacted() {
            return raw.to_string();
        }
        let chars: Vec<char> = raw.chars().collect();
        if chars.len() <= 4 {
            return "*".repeat(chars.len());
        }
        let keep = chars.len() - 4;
        let mut out = "*".repeat(keep);
        out.extend(&chars[keep..]);
        out
    }

    /// Replace a hostname with a stable pseudonym derived from it, so that
    /// grouping and counting still work after redaction.
    pub fn hostname(self, raw: &str) -> String {
        if !self.is_redacted() {
            return raw.to_string();
        }
        format!("device-{}", &fingerprint(&[raw])[..8])
    }

    /// Under redaction, keep only the executable's basename so process paths
    /// don't leak user home-directory structure; full path otherwise.
    ///
    /// `/home/alice/bin/agent` -> `agent`
    pub fn exe(self, raw: &str) -> String {
        if !self.is_redacted() {
            return raw.to_string();
        }
        std::path::Path::new(raw)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "**".to_string())
    }
}

/// FNV-1a 64-bit hash rendered as 16 lowercase hex characters.
///
/// Stable across runs and platforms, and cheap. It is an identifier, not a
/// secret: do not use it to hide low-entropy inputs from an attacker.
pub fn fingerprint(parts: &[&str]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        // Domain separator so ("ab","c") and ("a","bc") differ.
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_privacy_is_identity() {
        assert_eq!(Privacy::Full.mac("aa:bb:cc:dd:ee:ff"), "aa:bb:cc:dd:ee:ff");
        assert_eq!(Privacy::Full.secret("ABC123"), "ABC123");
        assert_eq!(Privacy::Full.hostname("box"), "box");
    }

    #[test]
    fn redaction_keeps_oui_and_tail() {
        assert_eq!(
            Privacy::Redacted.mac("aa:bb:cc:dd:ee:ff"),
            "aa:bb:cc:**:**:**"
        );
        assert_eq!(
            Privacy::Redacted.mac("AA-BB-CC-DD-EE-FF"),
            "AA-BB-CC-**-**-**"
        );
        assert_eq!(Privacy::Redacted.secret("W1KS427111E"), "*******111E");
        assert_eq!(Privacy::Redacted.secret("abc"), "***");
        assert!(
            Privacy::Redacted
                .hostname("laptop-01")
                .starts_with("device-")
        );
    }

    #[test]
    fn fingerprint_is_stable_and_order_sensitive() {
        assert_eq!(fingerprint(&["a", "b"]), fingerprint(&["a", "b"]));
        assert_ne!(fingerprint(&["a", "b"]), fingerprint(&["b", "a"]));
        assert_ne!(fingerprint(&["ab", "c"]), fingerprint(&["a", "bc"]));
        assert_eq!(fingerprint(&["a", "b"]).len(), 16);
    }
}
