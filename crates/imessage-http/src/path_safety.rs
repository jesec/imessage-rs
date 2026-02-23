use std::path::{Component, Path, PathBuf};

use crate::middleware::error::AppError;

/// Keep only a safe basename for filesystem writes.
pub fn sanitize_filename(raw: &str, fallback: &str) -> String {
    let normalized = raw.trim().replace('\\', "/");
    let candidate = Path::new(&normalized)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .trim();

    let cleaned: String = candidate
        .chars()
        .filter(|c| !c.is_control() && *c != '/' && *c != '\\')
        .collect();

    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        fallback.to_string()
    } else {
        cleaned
    }
}

/// Build a Content-Disposition-safe filename (ASCII token chars only).
pub fn sanitize_header_filename(raw: &str, fallback: &str) -> String {
    let base = sanitize_filename(raw, fallback);
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();

    if cleaned.is_empty() || cleaned.chars().all(|c| c == '_') {
        fallback.to_string()
    } else {
        cleaned
    }
}

/// Validate that a user-supplied value is a single safe path component.
pub fn sanitize_path_component(raw: &str, field_name: &str) -> Result<String, AppError> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(AppError::bad_request(&format!("{field_name} is required")));
    }
    if value.chars().any(char::is_control) {
        return Err(AppError::bad_request(&format!(
            "{field_name} contains invalid control characters"
        )));
    }
    if value.contains('/') || value.contains('\\') {
        return Err(AppError::bad_request(&format!(
            "{field_name} must not contain path separators"
        )));
    }
    if Path::new(value).is_absolute() {
        return Err(AppError::bad_request(&format!(
            "{field_name} must not be an absolute path"
        )));
    }

    let mut normal_components = 0usize;
    for component in Path::new(value).components() {
        match component {
            Component::Normal(_) => normal_components += 1,
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(AppError::bad_request(&format!(
                    "{field_name} must not contain path traversal segments"
                )));
            }
        }
    }

    if normal_components != 1 {
        return Err(AppError::bad_request(&format!(
            "{field_name} must be a single path component"
        )));
    }

    Ok(value.to_string())
}

/// Resolve a relative path under a base directory while rejecting traversal.
pub fn resolve_relative_path_in_base(
    base: &Path,
    relative: &str,
    field_name: &str,
) -> Result<PathBuf, AppError> {
    let relative = relative.trim();
    if relative.is_empty() {
        return Err(AppError::bad_request(&format!("{field_name} is required")));
    }

    let relative_path = Path::new(relative);
    if relative_path.is_absolute() {
        return Err(AppError::bad_request(&format!(
            "{field_name} must be a relative path"
        )));
    }

    let mut resolved = base.to_path_buf();
    let mut normal_components = 0usize;

    for component in relative_path.components() {
        match component {
            Component::Normal(seg) => {
                normal_components += 1;
                resolved.push(seg);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(AppError::bad_request(&format!(
                    "{field_name} must not contain path traversal segments"
                )));
            }
        }
    }

    if normal_components == 0 {
        return Err(AppError::bad_request(&format!(
            "{field_name} must contain at least one path segment"
        )));
    }

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_filename_strips_path_parts() {
        assert_eq!(sanitize_filename("../a/b.txt", "fallback"), "b.txt");
        assert_eq!(sanitize_filename("C:\\tmp\\x.png", "fallback"), "x.png");
        assert_eq!(sanitize_filename("..", "fallback"), "fallback");
    }

    #[test]
    fn sanitize_path_component_rejects_traversal() {
        assert!(sanitize_path_component("../x", "attachmentGuid").is_err());
        assert!(sanitize_path_component("a/b", "attachmentGuid").is_err());
        assert!(sanitize_path_component("/tmp/x", "attachmentGuid").is_err());
    }

    #[test]
    fn resolve_relative_path_rejects_parent() {
        let base = Path::new("/tmp/base");
        assert!(resolve_relative_path_in_base(base, "../x", "attachment").is_err());
        assert!(resolve_relative_path_in_base(base, "/tmp/x", "attachment").is_err());
    }
}
