// Native save dialog for user-exported text (history and dictionary).
//
// The renderer only supplies content and a suggested filename. The OS dialog
// chooses the destination, so the main window needs no fs plugin grant.

use std::path::Path;
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;

const ALLOWED_EXTENSIONS: [&str; 3] = ["txt", "csv", "json"];

fn parse_export_name(filename: &str) -> Result<(String, String), String> {
    let name = Path::new(filename)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Invalid export filename".to_string())?;
    if name.len() > 200
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ' '))
    {
        return Err("Invalid export filename".to_string());
    }
    let extension = Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| "Export filename needs a .txt, .csv, or .json extension".to_string())?;
    if !ALLOWED_EXTENSIONS.contains(&extension.as_str()) {
        return Err("Export filename needs a .txt, .csv, or .json extension".to_string());
    }
    Ok((name.to_string(), extension))
}

/// Save text to a user-picked path. Returns `false` when the dialog is
/// cancelled; write and dialog failures are returned so callers can toast.
#[tauri::command]
pub async fn save_export_file(
    window: tauri::Window,
    filename: String,
    content: String,
) -> Result<bool, String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
    let (filename, extension) = parse_export_name(&filename)?;
    let target = window
        .app_handle()
        .dialog()
        .file()
        .set_file_name(filename)
        .add_filter(extension.to_ascii_uppercase(), &[extension.as_str()])
        .blocking_save_file();
    let Some(target) = target else {
        return Ok(false);
    };
    let path = target
        .into_path()
        .map_err(|_| "Selected save location is not a local file".to_string())?;
    std::fs::write(&path, content)
        .map_err(|e| format!("Could not save {}: {e}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::parse_export_name;

    #[test]
    fn accepts_known_export_names_and_strips_directories() {
        assert_eq!(
            parse_export_name("fluence-history-20260309-070503.txt"),
            Ok((
                "fluence-history-20260309-070503.txt".to_string(),
                "txt".to_string()
            ))
        );
        assert_eq!(
            parse_export_name(r"C:\Users\u\fluence-dictionary.json"),
            Ok(("fluence-dictionary.json".to_string(), "json".to_string()))
        );
        assert_eq!(
            parse_export_name("EXPORT.CSV"),
            Ok(("EXPORT.CSV".to_string(), "csv".to_string()))
        );
    }

    #[test]
    fn rejects_missing_unsupported_or_unsafe_export_names() {
        for name in [
            "",
            ".",
            "..",
            "fluence",
            "fluence.exe",
            "fluence.txt.exe",
            "fluence?.txt",
        ] {
            assert!(
                parse_export_name(name).is_err(),
                "{name:?} should be rejected"
            );
        }
    }
}
