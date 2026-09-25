//! On Windows, give `disktree.exe` its icon and version information.
//!
//! An executable that says what it is — product, description, version,
//! copyright — is what Explorer, the taskbar and `SmartScreen` show, and
//! antivirus heuristics distrust a nameless one. The manifest (DPI
//! awareness, `asInvoker`) is GPUI's to embed, so none is added here: a
//! second one would not link.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/disktree.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        windows::embed();
    }
}

#[cfg(windows)]
mod windows {
    pub fn embed() {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("../../assets/disktree.ico")
            .set("ProductName", "disktree")
            .set("FileDescription", "disktree: see what fills your disk")
            .set("CompanyName", "disktree contributors")
            // As LICENSE has it.
            .set(
                "LegalCopyright",
                "Copyright (c) 2026 Tobi L\u{fc}tke, (c) 2026 menzew. MIT.",
            )
            .set("OriginalFilename", "disktree.exe")
            .set("InternalName", "disktree");
        // The version fields come from the crate's version.
        resource.compile().expect("embed the Windows resources");
    }
}
