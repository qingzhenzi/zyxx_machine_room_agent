//! Export/import commands that touch the filesystem on the user's behalf.

use tauri::Manager;

use crate::error::{AppError, AppResult};

/// Replace every character that is unsafe in a file name with `-`.
///
/// Only ASCII alphanumerics and `-`, `_`, `.` are kept, so the result can never
/// contain a path separator and cannot escape the download directory.
pub fn sanitize_file_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect()
}

/// Write exported text (reports, CSV) to the user's Downloads directory.
///
/// Keeping this in Rust avoids granting the webview broad filesystem access.
#[tauri::command]
pub fn save_text_file(app: tauri::AppHandle, name: String, contents: String) -> AppResult<String> {
    let safe_name = sanitize_file_name(&name);

    let directory = app.path().download_dir()?;
    let path = directory.join(safe_name);
    std::fs::write(&path, contents)?;

    Ok(path.display().to_string())
}

/// Read a user-selected JSON import file without granting broad FS access.
#[tauri::command]
pub fn read_import_file(path: String) -> AppResult<String> {
    let path = std::path::PathBuf::from(path);
    if path.extension().and_then(|value| value.to_str()) != Some("json") {
        return Err(AppError::msg("Only .json files can be imported"));
    }
    let metadata = std::fs::metadata(&path)?;
    if metadata.len() > 2 * 1024 * 1024 {
        return Err(AppError::msg("Import file is larger than 2 MB"));
    }
    Ok(std::fs::read_to_string(&path)?)
}

#[cfg(test)]
mod tests {
    use super::sanitize_file_name;

    #[test]
    fn replaces_spaces_and_path_separators() {
        assert_eq!(
            sanitize_file_name("quarterly report/2024.csv"),
            "quarterly-report-2024.csv"
        );
    }

    #[test]
    fn replaces_angle_brackets() {
        assert_eq!(sanitize_file_name("<Q3> report.xlsx"), "-Q3--report.xlsx");
    }

    #[test]
    fn cannot_traverse_directories() {
        let safe = sanitize_file_name("../../etc/passwd");
        assert!(!safe.contains('/'));
        assert_eq!(safe, "..-..-etc-passwd");
    }

    #[test]
    fn keeps_allowed_ascii_characters() {
        assert_eq!(sanitize_file_name("aZ09-_.json"), "aZ09-_.json");
    }

    #[test]
    fn replaces_non_ascii_and_control_characters() {
        assert_eq!(sanitize_file_name("报表 2024\n.csv"), "---2024-.csv");
    }
}
