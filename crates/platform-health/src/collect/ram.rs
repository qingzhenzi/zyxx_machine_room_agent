//! Physical RAM inventory from the raw SMBIOS DMI table.
//!
//! Parsing `/sys/firmware/dmi/tables/DMI` directly avoids shelling out to
//! `dmidecode`. The table is root-only on most distributions, so an
//! unreadable file degrades to `denied` rather than an error.

use crate::Options;
use crate::model::RamSlot;
use crate::section::Collected;

#[cfg(target_os = "linux")]
use crate::privacy::Privacy;

#[cfg(target_os = "linux")]
const DMI_TABLE: &str = "/sys/firmware/dmi/tables/DMI";
/// SMBIOS structure type for a memory device.
#[cfg(target_os = "linux")]
const MEMORY_DEVICE: u8 = 17;

#[allow(unused_variables)]
pub(super) fn collect(opts: &Options) -> Collected<Vec<RamSlot>> {
    #[cfg(target_os = "linux")]
    {
        let table = match std::fs::read(DMI_TABLE) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return Collected::denied("read SMBIOS table (run as root)");
            }
            Err(e) => {
                return Collected::err(format!("read {DMI_TABLE}: {e}"));
            }
        };
        Collected::ok(parse_ram_slots(&table, opts.privacy))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Collected::unsupported("SMBIOS parse is Linux-only")
    }
}

/// Walk the SMBIOS table, emitting one [`RamSlot`] per Type 17 structure.
#[cfg(target_os = "linux")]
fn parse_ram_slots(table: &[u8], privacy: Privacy) -> Vec<RamSlot> {
    let mut slots = Vec::new();
    let mut offset = 0usize;

    while offset + 4 <= table.len() {
        let kind = table[offset];
        let len = table[offset + 1] as usize;
        // Corrupt header: stop rather than mis-parse the tail.
        if len < 4 || offset + len > table.len() {
            break;
        }
        let formatted = &table[offset..offset + len];

        // String-set follows the formatted area: strings NUL-terminated, the
        // whole set ended by an extra NUL (zero strings = a single NUL).
        let strings_start = offset + len;
        let mut cursor = strings_start;
        let strings_end;
        let next;
        if cursor < table.len() && table[cursor] == 0 {
            // Empty string-set.
            cursor += 1;
            strings_end = cursor;
            next = cursor;
        } else {
            while cursor + 1 < table.len() && !(table[cursor] == 0 && table[cursor + 1] == 0) {
                cursor += 1;
            }
            strings_end = cursor;
            next = if cursor + 1 < table.len() {
                cursor + 2
            } else {
                table.len()
            };
        }
        let strings = &table[strings_start..strings_end];

        if kind == MEMORY_DEVICE
            && let Some(slot) = parse_memory_device(formatted, strings, privacy)
        {
            slots.push(slot);
        }

        // Always advance at least past the header so we can never spin.
        offset = next.max(offset + len);
    }

    slots
}

/// Decode one Type 17 structure. `None` means the slot is empty (size 0) and
/// should not appear in the inventory.
#[cfg(target_os = "linux")]
fn parse_memory_device(f: &[u8], strings: &[u8], privacy: Privacy) -> Option<RamSlot> {
    let len = f.len();

    // Size (u16 @ 0x0C): 0 = no module installed.
    let raw_size = le16(f, 0x0C)?;
    if raw_size == 0 {
        return None;
    }
    let size_bytes = if raw_size == 0x7FFF && len > 0x1F {
        // Extended size (u32 @ 0x1C) is in MiB.
        le32(f, 0x1C).and_then(|mib| (mib != 0).then(|| u64::from(mib) * 1024 * 1024))
    } else if raw_size == 0xFFFF {
        None
    } else if raw_size & 0x8000 == 0 {
        Some(u64::from(raw_size) * 1024 * 1024) // value is MiB
    } else {
        Some(u64::from(raw_size & 0x7FFF) * 1024) // value is KiB
    };

    let speed_mts = if len > 0x16 {
        le16(f, 0x15).and_then(|s| (s != 0 && s != 0xFFFF).then(|| u32::from(s)))
    } else {
        None
    };

    let memory_type = if len > 0x12 {
        match f.get(0x12) {
            Some(&0x12) => Some("SDRAM"),
            Some(&0x18) => Some("DDR2"),
            Some(&0x19) => Some("DDR"),
            Some(&0x1A) => Some("DDR3"),
            Some(&0x1B) => Some("LPDDR3"),
            Some(&0x1C) => Some("DDR4"),
            Some(&0x1D) => Some("LPDDR4"),
            Some(&0x1E) => Some("DDR4"),
            Some(&0x22) => Some("DDR5"),
            Some(&0x23) => Some("LPDDR5"),
            _ => None,
        }
        .map(str::to_string)
    } else {
        None
    };

    let slot = if len > 0x10 {
        string_at(strings, f[0x10])
    } else {
        None
    };
    let manufacturer = if len > 0x17 {
        string_at(strings, f[0x17])
    } else {
        None
    };
    let serial = if len > 0x18 {
        string_at(strings, f[0x18]).map(|s| privacy.secret(&s))
    } else {
        None
    };
    let part_number = if len > 0x1A {
        string_at(strings, f[0x1A])
    } else {
        None
    };

    // ECC shows up as total width exceeding data width.
    let ecc = if len > 0x0B {
        match (le16(f, 0x08), le16(f, 0x0A)) {
            (Some(total), Some(data))
                if total != 0 && total != 0xFFFF && data != 0 && data != 0xFFFF =>
            {
                Some(total > data)
            }
            _ => None,
        }
    } else {
        None
    };

    Some(RamSlot {
        slot,
        size_bytes,
        speed_mts,
        memory_type,
        manufacturer,
        part_number,
        serial,
        ecc,
    })
}

/// Resolve a 1-based SMBIOS string index (0 = not specified) against the
/// structure's string-set.
#[cfg(target_os = "linux")]
fn string_at(strings: &[u8], index: u8) -> Option<String> {
    if index == 0 {
        return None;
    }
    let raw = strings.split(|b| *b == 0).nth(index as usize - 1)?;
    let text = String::from_utf8_lossy(raw).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Bounds-checked little-endian reads; never panic on truncated tables.
#[cfg(target_os = "linux")]
fn le16(buf: &[u8], off: usize) -> Option<u16> {
    let b = buf.get(off..off + 2)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

#[cfg(target_os = "linux")]
fn le32(buf: &[u8], off: usize) -> Option<u32> {
    let b = buf.get(off..off + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
