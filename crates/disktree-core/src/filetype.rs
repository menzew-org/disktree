//! What a file is, by its name, for people.
//!
//! The colour categories in [`crate::classify`] say what a *place* holds and
//! are inherited, so a Java archive in a Maven cache is coloured as cache.
//! That is right for the mosaic and wrong as an answer to "what is this
//! file": this module answers that, from the extension or a well-known name.

/// A short description of the file called `name`: `Java archive`,
/// `Disk image`, or `PARQUET file` for an extension it does not know.
pub fn describe(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    if let Some(known) = by_name(&lower) {
        return known.to_string();
    }
    // A leading dot is a hidden name, not an extension: `.bashrc`.
    let extension = lower
        .rsplit_once('.')
        .filter(|(stem, extension)| !stem.is_empty() && !extension.is_empty())
        .map(|(_, extension)| extension);
    match extension {
        Some(extension) => by_extension(extension).map_or_else(
            || format!("{} file", extension.to_ascii_uppercase()),
            str::to_string,
        ),
        None => "File".to_string(),
    }
}

fn by_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "makefile" | "justfile" | "rakefile" => "Build script",
        "dockerfile" | "containerfile" => "Container recipe",
        "license" | "licence" | "copying" => "License",
        "readme" => "Read-me",
        "pagefile.sys" | "swapfile.sys" => "Windows page file",
        "hiberfil.sys" => "Windows hibernation file",
        ".gitignore" | ".gitattributes" | ".gitmodules" => "Git settings",
        ".bashrc" | ".zshrc" | ".profile" | ".bash_profile" => "Shell settings",
        "cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml"
        | "poetry.lock" | "uv.lock" | "gemfile.lock" | "go.sum" => "Lock file",
        _ => return None,
    })
}

fn by_extension(extension: &str) -> Option<&'static str> {
    Some(match extension {
        // Archives and packages.
        "jar" | "war" | "ear" => "Java archive",
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst"
        | "lz4" | "cab" => "Archive",
        "whl" | "egg" => "Python package",
        "crate" => "Rust package",
        "nupkg" => "NuGet package",
        "deb" | "rpm" | "apk" | "pkg" | "msi" | "msix" | "appx" => {
            "Installer package"
        }
        // Disks and virtual machines.
        "iso" | "img" | "dmg" => "Disk image",
        "vhd" | "vhdx" | "vmdk" | "vdi" | "qcow2" => "Virtual disk",
        // Programs and libraries.
        "exe" | "com" => "Program",
        "dll" | "so" | "dylib" => "Shared library",
        "a" | "lib" | "rlib" => "Static library",
        "o" | "obj" => "Object file",
        "class" => "Java class",
        "pyc" | "pyo" => "Python bytecode",
        "wasm" => "WebAssembly module",
        "pdb" => "Debug symbols",
        "node" => "Node.js add-on",
        // Code and text.
        "rs" => "Rust source",
        "py" => "Python source",
        "js" | "mjs" | "cjs" => "JavaScript source",
        "ts" | "mts" | "cts" => "TypeScript source",
        "jsx" | "tsx" => "React source",
        "java" => "Java source",
        "kt" | "kts" => "Kotlin source",
        "go" => "Go source",
        "c" | "h" => "C source",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "C++ source",
        "cs" => "C# source",
        "swift" => "Swift source",
        "rb" => "Ruby source",
        "php" => "PHP source",
        "sh" | "bash" | "zsh" | "fish" => "Shell script",
        "ps1" | "psm1" => "PowerShell script",
        "bat" | "cmd" => "Batch script",
        "html" | "htm" => "Web page",
        "css" | "scss" | "sass" | "less" => "Stylesheet",
        "json" | "jsonl" | "ndjson" => "JSON data",
        "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "xml" | "plist" => {
            "Settings"
        }
        "md" | "markdown" | "rst" => "Markdown text",
        "txt" | "text" => "Text",
        "log" => "Log",
        "csv" | "tsv" => "Spreadsheet data",
        "ipynb" => "Notebook",
        "map" => "Source map",
        // Data.
        "db" | "sqlite" | "sqlite3" | "mdb" | "accdb" => "Database",
        "parquet" | "arrow" | "feather" | "avro" | "orc" => "Columnar data",
        "npy" | "npz" | "h5" | "hdf5" => "Array data",
        "safetensors" | "gguf" | "ckpt" | "pt" | "pth" | "onnx" => "AI model",
        "bin" | "dat" => "Binary data",
        "pack" | "idx" => "Git objects",
        "tmp" | "temp" => "Temporary file",
        "bak" | "old" => "Backup",
        "part" | "crdownload" | "download" => "Unfinished download",
        "dmp" | "mdmp" | "hprof" | "core" => "Crash dump",
        "etl" | "evtx" => "Windows event log",
        "ost" | "pst" => "Outlook mailbox",
        // Documents.
        "pdf" => "PDF document",
        "doc" | "docx" | "odt" | "rtf" | "pages" => "Document",
        "xls" | "xlsx" | "ods" | "numbers" => "Spreadsheet",
        "ppt" | "pptx" | "odp" | "key" => "Presentation",
        "epub" | "mobi" => "E-book",
        // Media.
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tif" | "tiff"
        | "heic" | "heif" | "avif" | "ico" | "svg" => "Image",
        "raw" | "cr2" | "cr3" | "nef" | "arw" | "dng" | "raf" => "Camera raw",
        "psd" | "psb" | "xcf" | "kra" | "afphoto" => "Image project",
        "mp4" | "mkv" | "mov" | "avi" | "webm" | "wmv" | "m4v" | "flv"
        | "m2ts" => "Video",
        "mp3" | "flac" | "wav" | "aac" | "m4a" | "ogg" | "opus" | "wma"
        | "aiff" => "Audio",
        "ttf" | "otf" | "woff" | "woff2" => "Font",
        "blend" | "fbx" | "stl" | "glb" | "gltf" | "usdz" => "3D model",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_described_by_what_they_are() {
        assert_eq!(describe("guava-33.0.jar"), "Java archive");
        assert_eq!(describe("IMG_2041.HEIC"), "Image", "any case");
        assert_eq!(describe("backup.tar.gz"), "Archive", "the last extension");
        assert_eq!(describe("Makefile"), "Build script");
        assert_eq!(describe("Cargo.lock"), "Lock file");
        assert_eq!(describe("hiberfil.sys"), "Windows hibernation file");
    }

    #[test]
    fn unknown_and_missing_extensions_still_say_something() {
        assert_eq!(describe("events.zarr"), "ZARR file");
        assert_eq!(describe("notes"), "File");
        assert_eq!(
            describe(".envrc"),
            "File",
            "a dot-name is not an extension"
        );
        assert_eq!(describe("trailing."), "File");
    }
}
