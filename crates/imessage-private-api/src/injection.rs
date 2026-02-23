/// Helper dylib injection into macOS apps (Messages.app, FaceTime.app, FindMy.app).
///
/// Kills any running instance of the target app, then relaunches it with
/// DYLD_INSERT_LIBRARIES pointing to the embedded helper dylib so it connects
/// back to our TCP server.
use imessage_core::config::AppPaths;
use tracing::{error, info, warn};

use crate::HELPER_DYLIB;
use crate::service::PrivateApiService;

/// Relaunch an app with the helper dylib injected.
/// Used by refresh endpoints that need to restart an app and wait for reconnection.
pub async fn relaunch_app_with_dylib(app_name: &str) -> Result<(), String> {
    let dylib_dir = AppPaths::user_data().join("private-api");
    let dylib_path = dylib_dir.join("imessage-helper.dylib");

    if !dylib_path.exists() {
        return Err(format!(
            "Helper dylib not found at {}",
            dylib_path.display()
        ));
    }

    let app_path = [
        format!("/System/Applications/{app_name}.app/Contents/MacOS/{app_name}"),
        format!("/Applications/{app_name}.app/Contents/MacOS/{app_name}"),
    ]
    .iter()
    .find(|p| std::path::Path::new(p).exists())
    .cloned()
    .ok_or_else(|| format!("{app_name}.app binary not found"))?;

    info!("Relaunching {app_name}.app with helper dylib...");
    let child = tokio::process::Command::new(&app_path)
        .env("DYLD_INSERT_LIBRARIES", &dylib_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn {app_name}.app: {e}"))?;

    // Log if the process exits quickly (crash or rejection)
    let log_name = app_name.to_string();
    tokio::spawn(async move {
        match child.wait_with_output().await {
            Ok(output) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    warn!(
                        "{}.app exited with status {} after relaunch. stderr: {}",
                        log_name, output.status, stderr
                    );
                }
            }
            Err(e) => warn!("Error waiting for {}.app after relaunch: {e}", log_name),
        }
    });

    // Hide the app after 5 seconds
    let hide_name = app_name.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let script = format!(
            "tell application \"System Events\" to set visible of process \"{}\" to false",
            hide_name
        );
        let _ = tokio::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
            .await;
    });

    Ok(())
}

pub async fn inject_app_dylib(service: &PrivateApiService, app_name: &str) {
    // Write embedded dylib to disk (DYLD_INSERT_LIBRARIES requires a file path)
    let dylib_dir = AppPaths::user_data().join("private-api");
    if let Err(e) = std::fs::create_dir_all(&dylib_dir) {
        error!("Failed to create dylib directory: {e}");
        return;
    }
    let dylib_path = dylib_dir.join("imessage-helper.dylib");
    if let Err(e) = std::fs::write(&dylib_path, HELPER_DYLIB) {
        error!("Failed to write helper dylib: {e}");
        return;
    }

    // Find the app binary
    let app_bin = [
        format!("/System/Applications/{app_name}.app/Contents/MacOS/{app_name}"),
        format!("/Applications/{app_name}.app/Contents/MacOS/{app_name}"),
    ]
    .iter()
    .find(|p| std::path::Path::new(p).exists())
    .cloned();

    let Some(app_path) = app_bin else {
        warn!("{app_name}.app binary not found!");
        return;
    };

    info!("Injecting helper dylib into {}.app", app_name);

    let mut failure_count = 0u32;
    let mut last_error_time = std::time::Instant::now();

    while failure_count < 5 {
        // Kill existing process
        let _ = tokio::process::Command::new("killall")
            .arg(app_name)
            .output()
            .await;
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        // Check if we should stop (service might be shut down)
        if service.is_connected().await {
            info!("{app_name} helper already connected, skipping injection");
            return;
        }

        info!("Launching {app_name}.app with DYLD_INSERT_LIBRARIES...");
        let result = tokio::process::Command::new(&app_path)
            .env("DYLD_INSERT_LIBRARIES", &dylib_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn();

        match result {
            Ok(mut child) => {
                // Hide the app after 5 seconds
                let hide_name = app_name.to_string();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    let script = format!(
                        "tell application \"System Events\" to set visible of process \"{}\" to false",
                        hide_name
                    );
                    let _ = tokio::process::Command::new("osascript")
                        .arg("-e")
                        .arg(&script)
                        .output()
                        .await;
                });

                // Wait for process to exit
                match child.wait().await {
                    Ok(status) if status.success() => {
                        info!("{app_name}.app exited cleanly, restarting dylib...");
                        failure_count = 0;
                    }
                    Ok(status) => {
                        warn!("{app_name}.app exited with status: {status}");
                        if last_error_time.elapsed().as_secs() > 15 {
                            failure_count = 0;
                        }
                        failure_count += 1;
                        last_error_time = std::time::Instant::now();
                    }
                    Err(e) => {
                        warn!("Error waiting for {app_name}.app: {e}");
                        if last_error_time.elapsed().as_secs() > 15 {
                            failure_count = 0;
                        }
                        failure_count += 1;
                        last_error_time = std::time::Instant::now();
                    }
                }
            }
            Err(e) => {
                error!("Failed to spawn {app_name}.app: {e}");
                failure_count += 1;
                last_error_time = std::time::Instant::now();
            }
        }
    }

    if failure_count >= 5 {
        error!("Failed to start {app_name}.app with dylib after 5 attempts, giving up");
    }
}
