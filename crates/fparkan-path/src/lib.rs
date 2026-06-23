#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_precision_loss,
        clippy::expect_used,
        clippy::float_cmp,
        clippy::identity_op,
        clippy::too_many_lines,
        clippy::uninlined_format_args,
        clippy::map_unwrap_or,
        clippy::needless_raw_string_hashes,
        clippy::semicolon_if_nothing_returned,
        clippy::type_complexity,
        clippy::panic,
        clippy::unwrap_used
    )
)]
//! Legacy path normalization and ASCII lookup semantics.

use std::fmt;
use std::path::{Path, PathBuf};

/// Original bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OriginalPathBytes(pub Vec<u8>);

impl OriginalPathBytes {
    /// Returns the preserved byte image.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Returns the preserved byte image as an owned vector.
    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.0
    }
}

/// Normalized relative path.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct NormalizedPath {
    raw: Vec<u8>,
    display: String,
}

impl NormalizedPath {
    /// Returns string view.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.display
    }

    /// Returns normalized byte view.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.raw
    }

    /// Returns an OS path owned path buffer.
    #[must_use]
    pub fn as_path(&self) -> PathBuf {
        as_os_path_from_bytes(&self.raw)
    }
}

/// Normalized path paired with its original byte image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedPathWithOriginal {
    normalized: NormalizedPath,
    original: OriginalPathBytes,
}

impl NormalizedPathWithOriginal {
    /// Returns normalized path.
    #[must_use]
    pub fn normalized(&self) -> &NormalizedPath {
        &self.normalized
    }

    /// Returns original path bytes.
    #[must_use]
    pub fn original(&self) -> &OriginalPathBytes {
        &self.original
    }

    /// Splits into normalized and original path parts.
    #[must_use]
    pub fn into_parts(self) -> (NormalizedPath, OriginalPathBytes) {
        (self.normalized, self.original)
    }
}

/// ASCII lookup key.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LookupKey(pub Vec<u8>);

/// Resource name bytes.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ResourceName(pub Vec<u8>);

/// Path policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathPolicy {
    /// Strict legacy relative resource path.
    StrictLegacy,
    /// Host compatible relative path.
    HostCompatible,
}

/// Path error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PathError {
    /// Empty path.
    Empty,
    /// Embedded NUL.
    EmbeddedNul,
    /// Absolute path.
    Absolute,
    /// Parent traversal.
    ParentTraversal,
    /// Host path escape.
    EscapesRoot,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "path is empty"),
            Self::EmbeddedNul => write!(f, "path contains an embedded NUL byte"),
            Self::Absolute => write!(f, "path must be relative and cannot be absolute"),
            Self::ParentTraversal => write!(f, "path attempts to traverse outside its root"),
            Self::EscapesRoot => write!(f, "normalized path escapes the configured root"),
        }
    }
}

impl std::error::Error for PathError {}

/// Normalizes a relative path.
///
/// # Errors
///
/// Returns [`PathError`] when the input is empty, absolute, contains an
/// embedded NUL, attempts parent traversal, or has an invalid drive prefix.
pub fn normalize_relative(raw: &[u8], policy: PathPolicy) -> Result<NormalizedPath, PathError> {
    if raw.is_empty() {
        return Err(PathError::Empty);
    }
    if raw.contains(&0) {
        return Err(PathError::EmbeddedNul);
    }
    if raw.starts_with(b"/") || raw.starts_with(b"\\") || has_drive_prefix(raw) {
        return Err(PathError::Absolute);
    }
    let mut parts = Vec::new();
    for part in raw.split(|byte| *byte == b'/' || *byte == b'\\') {
        if part.is_empty() || part == b"." {
            if policy == PathPolicy::StrictLegacy {
                return Err(PathError::ParentTraversal);
            }
            continue;
        }
        if part == b".." {
            return Err(PathError::ParentTraversal);
        }
        if policy == PathPolicy::StrictLegacy && part.contains(&b':') {
            return Err(PathError::Absolute);
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return Err(PathError::Empty);
    }
    let mut normalized = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            normalized.push(b'/');
        }
        normalized.extend_from_slice(part);
    }
    let display = String::from_utf8_lossy(&normalized).into_owned();
    Ok(NormalizedPath {
        raw: normalized,
        display,
    })
}

/// Normalizes a relative path while preserving its original bytes.
///
/// # Errors
///
/// Returns [`PathError`] under the same conditions as [`normalize_relative`].
pub fn normalize_relative_with_original(
    raw: &[u8],
    policy: PathPolicy,
) -> Result<NormalizedPathWithOriginal, PathError> {
    let normalized = normalize_relative(raw, policy)?;
    Ok(NormalizedPathWithOriginal {
        normalized,
        original: OriginalPathBytes(raw.to_vec()),
    })
}

fn has_drive_prefix(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

/// Builds an ASCII-only casefold lookup key.
#[must_use]
pub fn ascii_lookup_key(raw: &[u8]) -> LookupKey {
    LookupKey(raw.iter().map(u8::to_ascii_uppercase).collect())
}

/// Ensures relative path does not escape.
///
/// # Errors
///
/// Returns [`PathError::ParentTraversal`] when a normalized segment attempts
/// to address a parent directory.
pub fn reject_escape(rel: &NormalizedPath) -> Result<(), PathError> {
    if rel
        .as_bytes()
        .split(|byte| *byte == b'/')
        .any(|part| part == b"..")
    {
        Err(PathError::ParentTraversal)
    } else {
        Ok(())
    }
}

/// Joins normalized path under root.
///
/// # Errors
///
/// Returns [`PathError`] if the normalized path fails the escape check.
pub fn join_under(root: &Path, rel: &NormalizedPath) -> Result<PathBuf, PathError> {
    reject_escape(rel)?;
    Ok(root.join(rel.as_path()))
}

#[cfg(unix)]
fn as_os_path_from_bytes(raw: &[u8]) -> PathBuf {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    PathBuf::from(OsString::from_vec(raw.to_vec()))
}

#[cfg(not(unix))]
fn as_os_path_from_bytes(raw: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(raw).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_separators() {
        let p = normalize_relative(b"DATA\\MAPS/INTRO/Land.msh", PathPolicy::StrictLegacy)
            .expect("path");
        assert_eq!(p.as_str(), "DATA/MAPS/INTRO/Land.msh");
    }

    #[test]
    fn rejects_escape() {
        assert_eq!(
            normalize_relative(b"DATA/../secret", PathPolicy::StrictLegacy),
            Err(PathError::ParentTraversal)
        );
    }

    #[test]
    fn rejects_absolute_drive_and_nul_paths() {
        assert_eq!(
            normalize_relative(b"/DATA/MAPS", PathPolicy::StrictLegacy),
            Err(PathError::Absolute)
        );
        assert_eq!(
            normalize_relative(b"C:\\DATA\\MAPS", PathPolicy::StrictLegacy),
            Err(PathError::Absolute)
        );
        assert_eq!(
            normalize_relative(b"DATA\0MAPS", PathPolicy::StrictLegacy),
            Err(PathError::EmbeddedNul)
        );
    }

    #[test]
    fn path_error_display_is_actionable() {
        assert_eq!(
            PathError::ParentTraversal.to_string(),
            "path attempts to traverse outside its root"
        );
        assert_eq!(
            PathError::EmbeddedNul.to_string(),
            "path contains an embedded NUL byte"
        );
    }

    #[test]
    fn strict_legacy_rejects_host_only_segments() {
        assert_eq!(
            normalize_relative(b"./DATA/MAPS", PathPolicy::StrictLegacy),
            Err(PathError::ParentTraversal)
        );
        assert_eq!(
            normalize_relative(b"DATA//MAPS", PathPolicy::StrictLegacy),
            Err(PathError::ParentTraversal)
        );
        assert_eq!(
            normalize_relative(b"DATA/stream:name", PathPolicy::StrictLegacy),
            Err(PathError::Absolute)
        );

        let host = normalize_relative(b"./DATA//MAPS", PathPolicy::HostCompatible).expect("host");
        assert_eq!(host.as_str(), "DATA/MAPS");
    }

    #[test]
    fn join_under_keeps_normalized_path_below_root() {
        let rel = normalize_relative(b"DATA/MAPS/Land.map", PathPolicy::StrictLegacy)
            .expect("relative path");
        let joined = join_under(Path::new("/game"), &rel).expect("join");

        assert_eq!(joined, PathBuf::from("/game/DATA/MAPS/Land.map"));
    }

    #[test]
    fn ascii_casefold_does_not_unicode_fold() {
        assert_eq!(ascii_lookup_key(b"AbZ\xD0"), LookupKey(b"ABZ\xD0".to_vec()));
    }

    #[test]
    fn non_ascii_original_bytes_remain_stable() {
        let raw = "DATA/Тест.bin".as_bytes();
        let path = normalize_relative_with_original(raw, PathPolicy::StrictLegacy)
            .expect("path with non-ASCII UTF-8");

        assert_eq!(path.normalized().as_str().as_bytes(), raw);
        assert_eq!(path.original().as_bytes(), raw);
        assert_eq!(&ascii_lookup_key(raw).0[5..13], &raw[5..13]);
    }

    #[test]
    fn accepts_non_utf8_legacy_bytes() {
        let path = normalize_relative(b"DATA/\xFF.bin", PathPolicy::HostCompatible)
            .expect("raw legacy bytes");

        assert_eq!(path.as_str(), "DATA/\u{FFFD}.bin");
    }

    #[test]
    fn original_separators_and_raw_bytes_are_preserved() {
        let raw = b"DATA\\Maps/Intro\\Land.msh";
        let path = normalize_relative_with_original(raw, PathPolicy::StrictLegacy).expect("path");

        assert_eq!(path.normalized().as_str(), "DATA/Maps/Intro/Land.msh");
        assert_eq!(path.original().as_bytes(), raw);
    }
}
