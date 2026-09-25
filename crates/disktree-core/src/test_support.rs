//! Helpers the tests share across platforms.

use std::path::Path;

/// Make `link` a link to the directory `target`: a symlink on Unix, a
/// junction on Windows. A junction is what Windows itself uses for links in
/// a profile, and unlike a symlink it needs no privilege to create.
pub fn link_dir(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("symlink");
    #[cfg(windows)]
    {
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .expect("run mklink");
        assert!(
            output.status.success(),
            "mklink /J: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
