use std::process::Command;

/// IDE configuration: name and corresponding Bundle ID
const IDE_CONFIGS: &[(&str, &str)] = &[
    ("Visual Studio Code", "com.microsoft.VSCode"),
    ("Cursor", "com.todesktop.230313mzl4w4u92"),
    ("Windsurf", "com.exafunction.windsurf"),
    ("Antigravity", "com.google.antigravity"),
    ("Codex", "com.openai.codex"), // Tentative, to be corrected after user confirmation
];

/// Detect running IDEs
pub fn detect_running_ides() -> Vec<String> {
    let mut running = Vec::new();

    for &(name, bundle_id) in IDE_CONFIGS {
        let script = format!(
            r#"
            tell application "System Events"
                if exists (every application process whose bundle identifier is "{}") then
                    return "true"
                else
                    return "false"
                end if
            end tell
            "#,
            bundle_id
        );

        if let Ok(output) = run_applescript(&script) {
            if output.trim() == "true" {
                running.push(name.to_string());
            }
        }
    }
    running
}

/// Reload specified IDE
pub fn reload_ide(name: &str, use_window_reload: bool) -> Result<(), String> {
    // Kill all codex processes (excluding Codex Switcher itself)
    let script = r#"
        for pid in $(pgrep -f codex 2>/dev/null); do
            cmd=$(ps -p "$pid" -o command= 2>/dev/null || true)
            case "$cmd" in
                *codex-switcher*|*Codex\ Switcher*|*codex_switcher*) continue ;;
            esac
            kill -9 "$pid" 2>/dev/null
        done
    "#;
    let output = Command::new("sh").arg("-c").arg(script).output();

    if let Ok(o) = output {
        if o.status.success() {
            println!("Killed all codex processes");
        }
    }

    // Optional: keep original AppleScript shortcut refresh mechanism as fallback, or return directly
    // Keep subsequent logic here to let IDE execute Reload Window / Restart Extension Host to ensure frontend view refreshes
    // If user only wants pkill, we could return Ok(()) directly. But per semantics "after switching account, if auto-reload IDE, directly call pkill -9 -f codex",
    // we use it as primary operation. Keep original keystroke simulation here for thoroughness.

    // Prefer attempting keystroke simulation command
    let bundle_id = IDE_CONFIGS
        .iter()
        .find(|&&(n, _)| n == name)
        .map(|&(_, b)| b)
        .ok_or_else(|| format!("IDE configuration not found for {}", name))?;

    let command_text = if use_window_reload {
        "Reload Window"
    } else {
        "Restart Extension Host"
    };

    // AppleScript: activate using bundle id and send command
    let script = format!(
        r#"
        tell application id "{}"
            activate
            delay 0.5
            tell application "System Events"
                keystroke "p" using {{command down, shift down}}
                delay 0.5
                keystroke "{}"
                delay 0.5
                keystroke return
            end tell
        end tell
        "#,
        bundle_id, command_text
    );

    match run_applescript(&script) {
        Ok(_) => Ok(()),
        Err(e) if e.contains("1002") || e.contains("not allowed") || e.contains("keystroke not allowed") =>
        {
            // Capture permission error, return friendly hint instead of direct error
            Err(
                "PERMISSION_DENIED: Accessibility permission required to reload window. Reload manually or grant permission in Settings."
                    .to_string(),
            )
        }
        Err(e) => Err(e),
    }
}

/// Remove Codex App quarantine attribute (fixes crashes)
pub fn remove_quarantine() -> Result<(), String> {
    let script = r#"
    do shell script "xattr -dr com.apple.quarantine /Applications/Codex.app" with administrator privileges
    "#;

    run_applescript(script).map(|_| ())
}

/// Execute AppleScript
fn run_applescript(script: &str) -> Result<String, String> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|e| format!("Failed to execute osascript: {}", e))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("AppleScript execution failed: {}", err));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
