//! The few path facts that differ between Unix and Windows.
//!
//! Everything else in the crate compares paths lexically, so the one rule
//! that matters is that every path it compares is spelled the same way:
//! canonical, and on Windows without the `\\?\` prefix `canonicalize` adds,
//! which the home directory, the environment and the user never write.

use std::path::{Component, Path, PathBuf, Prefix};

/// The user's home directory: `HOME` on Unix, the profile directory on
/// Windows. `None` when it cannot be determined or is empty.
pub fn home_dir() -> Option<PathBuf> {
    std::env::home_dir().filter(|home| !home.as_os_str().is_empty())
}

/// `path` resolved through symlinks, or `path` itself when that fails, in
/// the spelling the rest of the app uses.
pub fn canonical(path: &Path) -> PathBuf {
    path.canonicalize()
        .map_or_else(|_| path.to_path_buf(), |path| simplify(&path))
}

/// Drop the verbatim prefix from `\\?\C:\x` and `\\?\UNC\host\share\x`, so
/// they compare equal to `C:\x` and `\\host\share\x`. Anything else,
/// including every Unix path, is returned unchanged.
pub fn simplify(path: &Path) -> PathBuf {
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return path.to_path_buf();
    };
    let head = match prefix.kind() {
        Prefix::VerbatimDisk(letter) => {
            format!("{}:", char::from(letter))
        }
        Prefix::VerbatimUNC(host, share) => {
            format!(r"\\{}\{}", host.to_string_lossy(), share.to_string_lossy())
        }
        _ => return path.to_path_buf(),
    };
    let mut out = PathBuf::from(head);
    out.extend(components);
    out
}

/// The top of the volume `path` is on, lexically: `C:\` for `C:\Users\x`,
/// `\\host\share\` for a share. `None` for a path with no prefix, which is
/// every Unix path; there the mount table answers instead.
pub fn prefix_root(path: &Path) -> Option<PathBuf> {
    let simplified = simplify(path);
    let Some(Component::Prefix(prefix)) = simplified.components().next() else {
        return None;
    };
    let mut root = PathBuf::from(prefix.as_os_str());
    root.push(std::path::MAIN_SEPARATOR_STR);
    Some(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_paths_pass_through() {
        let path = Path::new("/home/tobi/.cache");
        assert_eq!(simplify(path), path);
        assert_eq!(prefix_root(path), None);
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_prefixes_are_dropped() {
        assert_eq!(
            simplify(Path::new(r"\\?\C:\Users\tobi")),
            Path::new(r"C:\Users\tobi")
        );
        assert_eq!(
            simplify(Path::new(r"\\?\UNC\nas\home\tobi")),
            Path::new(r"\\nas\home\tobi")
        );
        assert_eq!(
            prefix_root(Path::new(r"\\?\C:\Users\tobi")),
            Some(PathBuf::from(r"C:\"))
        );
    }

    #[test]
    fn a_canonical_path_compares_with_the_home_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let canonical = canonical(temp.path());
        assert!(canonical.is_absolute(), "{}", canonical.display());
        assert!(
            !canonical.to_string_lossy().starts_with(r"\\?\"),
            "{}",
            canonical.display()
        );
    }
}
