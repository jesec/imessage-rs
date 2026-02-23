/// macOS process execution: osascript, sips, afconvert, mdls, and shell commands.
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Result, bail};
use tokio::process::Command;
use tracing::warn;

/// Timeout for osascript execution (30 seconds).
const OSASCRIPT_TIMEOUT: Duration = Duration::from_secs(30);

/// Execute a shell command, returning stdout (or stderr if stdout is empty).
pub async fn exec_shell_command(cmd: &str) -> Result<String> {
    let output = Command::new("sh")
        .args(["-c", cmd])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let msg = if stderr.is_empty() { stdout } else { stderr };
        bail!("{msg}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if stdout.is_empty() {
        Ok(stderr)
    } else {
        Ok(stdout)
    }
}

/// Execute a multi-line AppleScript string via `osascript`.
///
/// Execute an AppleScript:
/// 1. Split on newlines
/// 2. Trim + escape double-quotes in each line
/// 3. Filter empty lines
/// 4. Pass each line as a separate `-e "line"` argument
pub async fn execute_applescript(script: &str) -> Result<String> {
    if script.trim().is_empty() {
        return Ok(String::new());
    }

    let lines: Vec<String> = script
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();

    if lines.is_empty() {
        return Ok(String::new());
    }

    let mut cmd = Command::new("osascript");
    for line in &lines {
        cmd.arg("-e");
        cmd.arg(line);
    }

    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    // Take stdout/stderr handles before waiting, so we can still kill on timeout.
    let mut stdout_handle = child.stdout.take();
    let mut stderr_handle = child.stderr.take();

    let status = match tokio::time::timeout(OSASCRIPT_TIMEOUT, child.wait()).await {
        Ok(result) => result?,
        Err(_) => {
            // Timeout — kill the hung osascript process
            let _ = child.kill().await;
            bail!("osascript timed out after {}s", OSASCRIPT_TIMEOUT.as_secs());
        }
    };

    // Read captured output
    use tokio::io::AsyncReadExt;
    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    if let Some(ref mut h) = stdout_handle {
        let _ = h.read_to_end(&mut stdout_buf).await;
    }
    if let Some(ref mut h) = stderr_handle {
        let _ = h.read_to_end(&mut stderr_buf).await;
    }

    let stdout = String::from_utf8_lossy(&stdout_buf).trim().to_string();
    let stderr = String::from_utf8_lossy(&stderr_buf).trim().to_string();

    if !status.success() {
        // Strip the "execution error: " prefix and ". (..." suffix
        let msg = extract_osa_error(&stderr);
        bail!("{msg}");
    }

    if stdout.is_empty() {
        Ok(stderr)
    } else {
        Ok(stdout)
    }
}

/// Execute an AppleScript with error handling. Logs warnings on failure.
pub async fn safe_execute_applescript(script: &str) -> Result<String> {
    match execute_applescript(script).await {
        Ok(output) => Ok(output),
        Err(e) => {
            warn!("AppleScript error: {e}");
            Err(e)
        }
    }
}

/// Extract a clean error message from osascript stderr.
fn extract_osa_error(stderr: &str) -> String {
    if let Some(rest) = stderr.strip_prefix("execution error: ") {
        // Strip trailing ". (error code)" suffix
        if let Some(idx) = rest.rfind(". (") {
            return rest[..idx].to_string();
        }
        return rest.to_string();
    }
    stderr.to_string()
}

// ---------------------------------------------------------------------------
// CLI tool wrappers
// ---------------------------------------------------------------------------

/// Convert HEIC/HEIF/TIFF image to JPEG using sips.
pub async fn convert_to_jpg(input_path: &str, output_path: &str) -> Result<()> {
    let real_path = imessage_core::utils::expand_tilde(input_path);
    let output = exec_shell_command(&format!(
        "/usr/bin/sips --setProperty \"format\" \"jpeg\" \"{}\" --out \"{}\"",
        real_path.display(),
        output_path
    ))
    .await?;

    if output.contains("Error:") {
        bail!("Failed to convert image to JPEG: {output}");
    }
    Ok(())
}

/// Resize an image using sips (either by width or height).
pub async fn resize_image(
    input_path: &str,
    output_path: &str,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<()> {
    let real_path = imessage_core::utils::expand_tilde(input_path);
    let resize_flag = if let Some(w) = width {
        format!("--resampleWidth {w}")
    } else if let Some(h) = height {
        format!("--resampleHeight {h}")
    } else {
        bail!("Must specify width or height for resize");
    };

    let output = exec_shell_command(&format!(
        "/usr/bin/sips --setProperty format jpeg {resize_flag} \"{}\" --out \"{}\"",
        real_path.display(),
        output_path
    ))
    .await?;

    if output.contains("Error:") {
        bail!("Failed to resize image: {output}");
    }
    Ok(())
}

/// Convert CAF audio to M4A/AAC using afconvert.
pub async fn convert_caf_to_m4a(input_path: &str, output_path: &str) -> Result<()> {
    let real_path = imessage_core::utils::expand_tilde(input_path);
    let output = exec_shell_command(&format!(
        "/usr/bin/afconvert -f m4af -d aac \"{}\" \"{}\"",
        real_path.display(),
        output_path
    ))
    .await?;

    if output.contains("Error:") {
        bail!("Failed to convert audio to M4A: {output}");
    }
    Ok(())
}

/// Convert MP3 to CAF for iMessage audio messages.
pub async fn convert_mp3_to_caf(input_path: &str, output_path: &str) -> Result<()> {
    let real_path = imessage_core::utils::expand_tilde(input_path);
    let output = exec_shell_command(&format!(
        "/usr/bin/afconvert -f caff -d LEI16@44100 -c 1 \"{}\" \"{}\"",
        real_path.display(),
        output_path
    ))
    .await?;

    if output.contains("Error:") {
        bail!("Failed to convert audio to CAF: {output}");
    }
    Ok(())
}

/// Get the iCloud account identifier from MobileMeAccounts.plist.
pub async fn get_icloud_account() -> Result<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let plist_path = format!("{}/Library/Preferences/MobileMeAccounts.plist", home);
    let output = exec_shell_command(&format!(
        "/usr/libexec/PlistBuddy -c \"Print :Accounts:0:AccountID\" \"{}\"",
        plist_path
    ))
    .await?;
    Ok(output.trim().to_string())
}

/// Get the system region/locale for phone number formatting.
pub async fn get_region() -> Result<String> {
    let output = exec_shell_command("defaults read -g AppleLanguages").await?;
    // Parse the first language entry, extract region code
    // Output looks like: (\n    "en-US",\n    "ja"\n)
    for line in output.lines() {
        let trimmed = line.trim().trim_matches('"').trim_matches(',');
        if trimmed.contains('-') {
            // e.g., "en-US" -> "US"
            if let Some(region) = trimmed.split('-').next_back() {
                return Ok(region.to_string());
            }
        }
    }
    Ok("US".to_string())
}

/// Check if SIP (System Integrity Protection) is disabled.
pub async fn is_sip_disabled() -> Result<bool> {
    let output = exec_shell_command("csrutil status").await?;
    Ok(output.contains("disabled"))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_osa_error_strips_prefix() {
        let e = extract_osa_error("execution error: Group chat does not exist. (-2700)");
        assert_eq!(e, "Group chat does not exist");
    }

    #[test]
    fn extract_osa_error_passthrough() {
        let e = extract_osa_error("some other error");
        assert_eq!(e, "some other error");
    }
}
