//! display collector: connected DRM connectors under `/sys/class/drm`, with
//! best-effort EDID name and physical size.
//!
//! Refresh rate and rotation are not exposed by sysfs without driver ioctls,
//! so they report `None` and `0`.

use crate::Options;
use crate::model::Display;
use crate::section::Collected;

#[allow(unused_variables)]
pub(super) fn collect(opts: &Options) -> Collected<Vec<Display>> {
    #[cfg(target_os = "linux")]
    {
        collect_linux()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Collected::unsupported("displays only read via DRM on Linux for now")
    }
}

/// Walk the DRM class for connected connectors.
#[cfg(target_os = "linux")]
fn collect_linux() -> Collected<Vec<Display>> {
    let base = std::path::Path::new("/sys/class/drm");
    let entries = match std::fs::read_dir(base) {
        Ok(e) => e,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Collected::unsupported("no DRM on this platform");
        }
        Err(err) => return Collected::denied(format!("cannot read /sys/class/drm: {err}")),
    };
    // read_dir order is unspecified; sort so "first connector" is stable.
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());

    let mut list: Vec<(String, Display)> = Vec::new();
    for entry in entries {
        let connector = entry.file_name().to_string_lossy().to_string();
        // Connectors look like `card0-eDP-1`; skip the bare `card0` node,
        // `renderD*` nodes and the `version` file.
        if !connector.starts_with("card") || !connector.contains('-') {
            continue;
        }
        let dir = entry.path();
        let status = std::fs::read_to_string(dir.join("status"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if status != "connected" {
            continue;
        }

        // First mode line is the preferred mode; interlaced modes end in `i`.
        let (width, height) = std::fs::read_to_string(dir.join("modes"))
            .ok()
            .and_then(|modes| modes.lines().next().and_then(parse_mode))
            .unwrap_or((0, 0));

        // EDID is optional: without it we still report the connected mode.
        let (name, physical_width_mm, physical_height_mm) = match std::fs::read(dir.join("edid")) {
            Ok(edid) if edid.len() >= 128 && has_edid_header(&edid) => (
                edid_monitor_name(&edid),
                // Bytes 21/22: screen size in centimetres -> millimetres.
                Some(edid[21] as u32 * 10),
                Some(edid[22] as u32 * 10),
            ),
            _ => (None, None, None),
        };

        list.push((
            connector,
            Display {
                name,
                width,
                height,
                physical_width_mm,
                physical_height_mm,
                refresh_rate: None,
                is_primary: false, // resolved once every connector is known
                rotation: 0,
            },
        ));
    }

    // Prefer the internal panel (eDP/LVDS) as primary; otherwise the first
    // connected connector wins.
    let primary = list
        .iter()
        .position(|(conn, _)| conn.contains("eDP") || conn.contains("LVDS"))
        .unwrap_or(0);

    let displays: Vec<Display> = list
        .into_iter()
        .enumerate()
        .map(|(i, (_, mut d))| {
            d.is_primary = i == primary;
            d
        })
        .collect();

    Collected::ok(displays)
}

/// Parse a DRM mode line such as `1920x1080` or `1920x1080i`.
#[cfg(target_os = "linux")]
fn parse_mode(line: &str) -> Option<(u32, u32)> {
    let line = line.trim();
    // Interlaced modes carry a trailing `i`.
    let line = line.strip_suffix('i').unwrap_or(line);
    let (w, h) = line.split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// EDID header: `00 FF FF FF FF FF FF 00`.
#[cfg(target_os = "linux")]
fn has_edid_header(edid: &[u8]) -> bool {
    edid.starts_with(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00])
}

/// Monitor name from the EDID 0xFC descriptor blocks (offsets 54/72/90/108):
/// up to 13 ASCII bytes terminated by a newline, then space/NUL padding.
#[cfg(target_os = "linux")]
fn edid_monitor_name(edid: &[u8]) -> Option<String> {
    for offset in [54usize, 72, 90, 108] {
        let Some(block) = edid.get(offset..offset + 18) else {
            continue;
        };
        if block[0] != 0 || block[1] != 0 || block[3] != 0xFC {
            continue;
        }
        let raw = &block[5..18];
        let end = raw.iter().position(|&b| b == b'\n').unwrap_or(raw.len());
        let text = String::from_utf8_lossy(&raw[..end]);
        let text = text.trim_matches('\0').trim();
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }
    None
}
