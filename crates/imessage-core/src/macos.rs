use std::sync::OnceLock;
use tracing::warn;

/// Cached macOS version, detected once at startup.
static MACOS_VERSION: OnceLock<MacOsVersion> = OnceLock::new();

/// Parsed macOS version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl MacOsVersion {
    pub fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Check if this version is >= the given version.
    pub fn is_at_least(&self, major: u32, minor: u32) -> bool {
        if self.major != major {
            return self.major > major;
        }
        self.minor >= minor
    }

    pub fn is_min_tahoe(&self) -> bool {
        self.is_at_least(26, 0)
    }
}

/// Check that the running macOS version is at least Sequoia (15.0).
/// Returns `Ok(())` if so, or an error message if not.
pub fn require_min_sequoia() -> Result<(), String> {
    let v = macos_version();
    if v.is_at_least(15, 0) {
        Ok(())
    } else {
        Err(format!(
            "imessage-rs requires macOS Sequoia (15.0) or newer, but detected macOS {v}"
        ))
    }
}

impl std::fmt::Display for MacOsVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Detect the macOS version by running `sw_vers -productVersion`.
fn detect_version() -> MacOsVersion {
    let output = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let version_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
            parse_version(&version_str).unwrap_or_else(|| {
                warn!("Failed to parse macOS version string: {version_str}");
                MacOsVersion::new(0, 0, 0)
            })
        }
        Ok(out) => {
            warn!(
                "sw_vers failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
            MacOsVersion::new(0, 0, 0)
        }
        Err(e) => {
            warn!("Failed to run sw_vers: {e}");
            MacOsVersion::new(0, 0, 0)
        }
    }
}

/// Parse a version string like "26.3.0" or "15.0" into a MacOsVersion.
fn parse_version(s: &str) -> Option<MacOsVersion> {
    let parts: Vec<&str> = s.split('.').collect();
    let major = parts.first()?.parse().ok()?;
    let minor = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let patch = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    Some(MacOsVersion::new(major, minor, patch))
}

/// Get the macOS version (cached, detected once).
pub fn macos_version() -> MacOsVersion {
    *MACOS_VERSION.get_or_init(detect_version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_three_part() {
        let v = parse_version("26.3.0").unwrap();
        assert_eq!(v.major, 26);
        assert_eq!(v.minor, 3);
        assert_eq!(v.patch, 0);
    }

    #[test]
    fn parse_two_part() {
        let v = parse_version("15.0").unwrap();
        assert_eq!(v.major, 15);
        assert_eq!(v.minor, 0);
        assert_eq!(v.patch, 0);
    }

    #[test]
    fn tahoe_version_flags() {
        let v = MacOsVersion::new(26, 3, 0);
        assert!(v.is_min_tahoe());
        assert!(v.is_at_least(15, 0));
    }

    #[test]
    fn sequoia_version_flags() {
        let v = MacOsVersion::new(15, 0, 0);
        assert!(!v.is_min_tahoe());
        assert!(v.is_at_least(15, 0));
    }

    #[test]
    fn require_min_sequoia_passes() {
        // This test runs on macOS >= Sequoia, so it should pass
        assert!(require_min_sequoia().is_ok());
    }

    #[test]
    fn display_format() {
        let v = MacOsVersion::new(26, 3, 0);
        assert_eq!(format!("{v}"), "26.3.0");
    }
}
