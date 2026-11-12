//! Minimal XML helpers shared by the config parser and the libvirt backend.
//!
//! Deliberately not a full parser: we only need to pull a handful of fields
//! out of libvirt domain documents, and to escape values we render into them.

/// Escape text for use in XML character data or a quoted attribute.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// Unescape the five predefined entities plus numeric character references.
pub fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        let Some(semi) = rest.find(';') else {
            out.push_str(rest);
            return out;
        };
        let entity = &rest[1..semi];
        let decoded = match entity {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(ch) => out.push(ch),
            None => out.push_str(&rest[..=semi]),
        }
        rest = &rest[semi + 1..];
    }
    out.push_str(rest);
    out
}

/// Text content of the first `<tag>` element, unescaped and trimmed.
pub fn text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let start = xml.find(&open)?;
    let after_open = xml[start..].find('>')? + start + 1;
    let close = format!("</{tag}>");
    let end = xml[after_open..].find(&close)? + after_open;
    let raw = xml[after_open..end].trim();
    if raw.is_empty() {
        return None;
    }
    Some(unescape(raw))
}

/// Value of an attribute inside one element tag string, unescaped.
pub fn attr(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=");
    let start = tag.find(&needle)? + needle.len();
    let quote = tag[start..].chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let body = &tag[start + 1..];
    let end = body.find(quote)?;
    Some(unescape(&body[..end]))
}

/// `<memory unit='KiB'>2048</memory>` → KiB.
pub fn memory_kib(xml: &str) -> Option<u64> {
    let open = xml.find("<memory")?;
    let tag_end = xml[open..].find('>')? + open;
    let unit = attr(&xml[open..tag_end], "unit").unwrap_or_else(|| "KiB".to_owned());
    let value: u64 = text(xml, "memory")?.parse().ok()?;
    Some(match unit.to_ascii_lowercase().as_str() {
        "b" => value / 1024,
        "kb" => value * 1000 / 1024,
        "kib" | "k" => value,
        "mb" => value * 1_000_000 / 1024,
        "mib" | "m" => value * 1024,
        "gb" => value * 1_000_000_000 / 1024,
        "gib" | "g" => value * 1024 * 1024,
        _ => value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_and_unescapes() {
        let raw = "a&b<c>d\"e'f";
        let escaped = escape(raw);
        assert_eq!(escaped, "a&amp;b&lt;c&gt;d&quot;e&apos;f");
        assert_eq!(unescape(&escaped), raw);
        assert_eq!(unescape("&#65;&#x42;&bogus;"), "AB&bogus;");
    }

    #[test]
    fn reads_attr() {
        let tag = "<graphics type='vnc' port=\"5900\" autoport='yes'>";
        assert_eq!(attr(tag, "type").as_deref(), Some("vnc"));
        assert_eq!(attr(tag, "port").as_deref(), Some("5900"));
        assert_eq!(attr(tag, "missing"), None);
    }

    #[test]
    fn normalizes_memory_units() {
        assert_eq!(memory_kib("<memory unit='MiB'>2</memory>"), Some(2048));
        assert_eq!(memory_kib("<memory>512</memory>"), Some(512));
        assert_eq!(memory_kib("<memory unit='GiB'>1</memory>"), Some(1_048_576));
    }
}
