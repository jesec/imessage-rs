use std::path::PathBuf;

pub fn is_not_empty(s: &str) -> bool {
    !s.trim().is_empty()
}

/// Sanitize a string for AppleScript: escape backslashes and double quotes.
pub fn escape_osa_exp(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Expand `~` to the user's home directory in file paths.
///
/// Handles `~/path`, bare `~`, and passes absolute paths through unchanged.
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix('~') {
        let home = home::home_dir().unwrap_or_default();
        home.join(rest.trim_start_matches('/'))
    } else {
        PathBuf::from(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_applescript() {
        assert_eq!(escape_osa_exp(r#"say "hello""#), r#"say \"hello\""#);
        assert_eq!(escape_osa_exp(r"path\to"), r"path\\to");
    }

    #[test]
    fn expand_tilde_replaces_home() {
        let result = expand_tilde("~/Documents/test.txt");
        let s = result.to_string_lossy();
        assert!(!s.starts_with('~'));
        assert!(s.ends_with("Documents/test.txt"));
    }

    #[test]
    fn expand_tilde_no_change_for_absolute() {
        let result = expand_tilde("/usr/local/bin");
        assert_eq!(result, PathBuf::from("/usr/local/bin"));
    }

    #[test]
    fn expand_tilde_bare() {
        let result = expand_tilde("~");
        assert!(!result.to_string_lossy().contains('~'));
        assert!(!result.to_string_lossy().is_empty());
    }
}
