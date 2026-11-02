pub fn resolve(preference: &str) -> String {
    if preference.trim().is_empty() || preference.eq_ignore_ascii_case("system") {
        normalize_tag(&system_tag())
    } else {
        normalize_tag(preference)
    }
}

fn normalize_tag(tag: &str) -> String {
    let lower = tag.trim().replace('_', "-").to_ascii_lowercase();
    let base = lower.split('-').next().unwrap_or("en");
    if base == "de" {
        "de".to_string()
    } else {
        "en".to_string()
    }
}

#[cfg(windows)]
fn system_tag() -> String {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buffer = [0u16; 85];
    let len = unsafe { GetUserDefaultLocaleName(&mut buffer) };
    if len > 0 {
        String::from_utf16_lossy(&buffer[..(len as usize).saturating_sub(1)])
    } else {
        "en".to_string()
    }
}

#[cfg(not(windows))]
fn system_tag() -> String {
    std::env::var("LANG")
        .or_else(|_| std::env::var("LC_ALL"))
        .unwrap_or_else(|_| "en".to_string())
}

pub fn tr<'a>(lang: &str, key: &'a str) -> &'a str {
    if lang != "de" {
        return key;
    }
    match key {
        "Fluence: AI Voice Typing" => "Fluence: KI-Spracheingabe",
        "Open Settings" => "Einstellungen öffnen",
        "Transcription History" => "Transkriptionsverlauf",
        "Recording Mode" => "Aufnahmemodus",
        "Push-to-Toggle" => "Drücken zum Umschalten",
        "Hold-to-Record" => "Gedrückt halten",
        "Recording in progress" => "Aufnahme läuft",
        "Quit Fluence" => "Fluence beenden",
        "Fluence Settings" => "Fluence-Einstellungen",
        "Welcome to Fluence" => "Willkommen bei Fluence",
        "Fluence is ready" => "Fluence ist bereit",
        "Running in the system tray. Press your hotkey to start voice typing." => "Fluence läuft im Infobereich. Drücken Sie Ihre Tastenkombination, um mit der Spracheingabe zu beginnen.",
        _ => key,
    }
}
