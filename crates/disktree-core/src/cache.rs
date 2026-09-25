//! The last finished scan of a root, kept on disk so the next start can show
//! it at once while a fresh walk runs.
//!
//! Only what a scan measures is stored: each entry's name and kind, and for
//! a leaf its bytes and last write. Everything derived — totals, direct
//! figures, order, colour, what is reclaimable — is recomputed on load by
//! the same [`aggregate`] and [`classify`] a scan uses, so a cached tree
//! cannot disagree with the rules of the version reading it. Leaf bytes are
//! stored after hardlink de-duplication, so a second name stays free.
//!
//! The format is private and versioned; a file that is from another version,
//! for other options, truncated or otherwise off is ignored, never trusted.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use crate::classify::classify;
use crate::scan::ScanOptions;
use crate::tree::{Node, NodeKind, aggregate};

const MAGIC: &[u8; 4] = b"DTRC";
/// Bump when the layout below changes.
const VERSION: u32 = 1;
/// Deeper than any real filesystem nests; a corrupt count cannot recurse
/// the reader into a stack overflow.
const MAX_DEPTH: usize = 4096;
/// Cached roots kept; the least recently written go first.
const KEEP: usize = 8;

/// A tree read back from the cache.
#[derive(Debug)]
pub struct Cached {
    pub tree: Node,
    /// Unix seconds when it was written, for "from 2 h ago".
    pub saved_at: i64,
}

/// Where caches live: `%LOCALAPPDATA%\disktree\cache` on Windows,
/// `$XDG_CACHE_HOME/disktree` or `~/.cache/disktree` elsewhere.
pub fn default_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(|local| PathBuf::from(local).join("disktree").join("cache"))
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .or_else(|| {
                crate::paths::home_dir().map(|home| home.join(".cache"))
            })
            .map(|cache| cache.join("disktree"))
    };
    base.filter(|dir| dir.is_absolute())
}

/// Whether a scan with these options is a whole tree worth caching. A
/// depth-limited scan has holes, and showing one as the last state of the
/// disk would be wrong.
pub const fn cacheable(options: &ScanOptions) -> bool {
    options.max_depth.is_none()
}

/// Read the cached tree for `root` scanned with `options`, ordered by
/// `options.metric`. `Ok(None)` when there is none, or it does not match.
pub fn load(
    dir: &Path,
    root: &Path,
    options: &ScanOptions,
) -> io::Result<Option<Cached>> {
    let key = key(root, options);
    let file = match File::open(dir.join(file_name(&key))) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let mut reader = Reader(BufReader::with_capacity(1 << 20, file));
    let mut magic = [0_u8; 4];
    reader.0.read_exact(&mut magic)?;
    if &magic != MAGIC || reader.varint()? != u64::from(VERSION) {
        return Ok(None);
    }
    // The file name is a hash; the full key inside rules out a collision.
    if reader.string()? != key {
        return Ok(None);
    }
    let saved_at = reader.signed()?;
    let mut tree = reader.node(0)?;
    aggregate(&mut tree, options.metric);
    classify(&mut tree);
    Ok(Some(Cached { tree, saved_at }))
}

/// Write `tree` as the cached scan of `root`. Written to a temporary file
/// and renamed into place, so a reader never sees half of one.
pub fn save(
    dir: &Path,
    root: &Path,
    options: &ScanOptions,
    tree: &Node,
    saved_at: i64,
) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let key = key(root, options);
    let path = dir.join(file_name(&key));
    let temporary = path.with_extension("tmp");
    {
        let mut writer = Writer(BufWriter::with_capacity(
            1 << 20,
            File::create(&temporary)?,
        ));
        writer.0.write_all(MAGIC)?;
        writer.varint(u64::from(VERSION))?;
        writer.string(&key)?;
        writer.signed(saved_at)?;
        writer.node(tree)?;
        writer.0.flush()?;
    }
    fs::rename(&temporary, &path)?;
    prune(dir);
    Ok(())
}

/// Everything that changes what a scan measures. The metric is left out:
/// it only orders the tree, and ordering is redone on load.
fn key(root: &Path, options: &ScanOptions) -> String {
    format!(
        "{}\0apparent={} hidden={} one_fs={} follow={} dedup={}",
        root.display(),
        options.apparent_size,
        options.include_hidden,
        options.one_filesystem,
        options.follow_links,
        options.dedup_hardlinks,
    )
}

/// FNV-1a, spelled out so the name of a cache file does not change with a
/// dependency's hasher.
fn file_name(key: &str) -> String {
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}.tree")
}

/// Keep the [`KEEP`] most recently written caches.
fn prune(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut caches: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|entry| {
            entry.path().extension().is_some_and(|ext| ext == "tree")
        })
        .filter_map(|entry| {
            let written = entry.metadata().ok()?.modified().ok()?;
            Some((written, entry.path()))
        })
        .collect();
    caches.sort_by_key(|(written, _)| std::cmp::Reverse(*written));
    for (_, stale) in caches.into_iter().skip(KEEP) {
        let _ = fs::remove_file(stale);
    }
}

const fn kind_tag(kind: NodeKind) -> u8 {
    match kind {
        NodeKind::Directory => 0,
        NodeKind::File => 1,
        NodeKind::Symlink => 2,
        NodeKind::Other => 3,
    }
}

/// Set on the tag of a directory that could not be read.
const READ_ERROR: u8 = 0x80;

struct Writer<W: Write>(W);

impl<W: Write> Writer<W> {
    /// LEB128: most sizes and counts fit in one to four bytes.
    fn varint(&mut self, mut value: u64) -> io::Result<()> {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                return self.0.write_all(&[byte]);
            }
            self.0.write_all(&[byte | 0x80])?;
        }
    }

    fn signed(&mut self, value: i64) -> io::Result<()> {
        // Zigzag, so small negative times stay short.
        self.varint(((value << 1) ^ (value >> 63)) as u64)
    }

    fn string(&mut self, value: &str) -> io::Result<()> {
        self.varint(value.len() as u64)?;
        self.0.write_all(value.as_bytes())
    }

    fn node(&mut self, node: &Node) -> io::Result<()> {
        let tag =
            kind_tag(node.kind) | if node.read_error { READ_ERROR } else { 0 };
        self.0.write_all(&[tag])?;
        self.string(&node.name)?;
        if node.is_dir() {
            self.varint(node.children.len() as u64)?;
            for child in &node.children {
                self.node(child)?;
            }
        } else {
            self.varint(node.own_bytes)?;
            self.signed(node.modified)?;
        }
        Ok(())
    }
}

struct Reader<R: Read>(R);

impl<R: Read> Reader<R> {
    fn byte(&mut self) -> io::Result<u8> {
        let mut byte = [0_u8];
        self.0.read_exact(&mut byte)?;
        Ok(byte[0])
    }

    fn varint(&mut self) -> io::Result<u64> {
        let mut value = 0_u64;
        for shift in (0..64).step_by(7) {
            let byte = self.byte()?;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(corrupt("a number runs past 64 bits"))
    }

    fn signed(&mut self) -> io::Result<i64> {
        let raw = self.varint()?;
        Ok((raw >> 1).cast_signed() ^ -(raw & 1).cast_signed())
    }

    fn string(&mut self) -> io::Result<String> {
        let length = usize::try_from(self.varint()?)
            .map_err(|_| corrupt("a name longer than memory"))?;
        // No name on any filesystem comes close; this bounds the allocation
        // a corrupt length could ask for.
        if length > 64 * 1024 {
            return Err(corrupt("a name longer than any filesystem allows"));
        }
        let mut bytes = vec![0_u8; length];
        self.0.read_exact(&mut bytes)?;
        String::from_utf8(bytes).map_err(|_| corrupt("a name is not UTF-8"))
    }

    fn node(&mut self, depth: usize) -> io::Result<Node> {
        if depth > MAX_DEPTH {
            return Err(corrupt("nested deeper than any filesystem"));
        }
        let tag = self.byte()?;
        let name = self.string()?;
        let kind = match tag & !READ_ERROR {
            0 => NodeKind::Directory,
            1 => NodeKind::File,
            2 => NodeKind::Symlink,
            3 => NodeKind::Other,
            _ => return Err(corrupt("an unknown kind of entry")),
        };
        if kind.is_dir() {
            let mut node = Node::directory(name);
            node.read_error = tag & READ_ERROR != 0;
            let count = self.varint()?;
            // Grown as children arrive, not reserved from a count that
            // might be corrupt.
            for _ in 0..count {
                node.children.push(self.node(depth + 1)?);
            }
            Ok(node)
        } else {
            let bytes = self.varint()?;
            let mut node = Node::entry(name, kind, bytes);
            node.modified = self.signed()?;
            Ok(node)
        }
    }
}

fn corrupt(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("cache: {what}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::scan;
    use tempfile::TempDir;

    fn fixture() -> TempDir {
        let temp = TempDir::new().expect("tempdir");
        for (path, bytes) in [
            ("a/one.bin", 1_000_usize),
            ("a/b/two.bin", 20_000),
            (".cache/big.bin", 300_000),
            ("naïve name.txt", 10),
        ] {
            let file = temp.path().join(path);
            fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
            fs::write(&file, vec![b'x'; bytes]).expect("write");
        }
        temp
    }

    fn options() -> ScanOptions {
        ScanOptions {
            apparent_size: true,
            ..ScanOptions::default()
        }
    }

    fn same_tree(left: &Node, right: &Node) {
        assert_eq!(left.name, right.name);
        assert_eq!(left.kind, right.kind);
        assert_eq!(left.bytes, right.bytes, "{}", left.name);
        assert_eq!(left.files, right.files, "{}", left.name);
        assert_eq!(left.dirs, right.dirs, "{}", left.name);
        assert_eq!(left.modified, right.modified, "{}", left.name);
        assert_eq!(left.category, right.category, "{}", left.name);
        assert_eq!(left.reclaim, right.reclaim, "{}", left.name);
        assert_eq!(left.children.len(), right.children.len(), "{}", left.name);
        for (left, right) in left.children.iter().zip(&right.children) {
            same_tree(left, right);
        }
    }

    #[test]
    fn a_saved_scan_reads_back_as_the_same_tree() {
        let temp = fixture();
        let dir = TempDir::new().expect("cache dir");
        let tree = scan(temp.path(), options()).expect("scan");

        save(dir.path(), temp.path(), &options(), &tree, 1234).expect("save");
        let cached = load(dir.path(), temp.path(), &options())
            .expect("load")
            .expect("a cache");

        assert_eq!(cached.saved_at, 1234);
        same_tree(&cached.tree, &tree);
    }

    #[test]
    fn other_options_or_roots_do_not_see_the_cache() {
        let temp = fixture();
        let dir = TempDir::new().expect("cache dir");
        let tree = scan(temp.path(), options()).expect("scan");
        save(dir.path(), temp.path(), &options(), &tree, 1).expect("save");

        let on_disk = ScanOptions {
            apparent_size: false,
            ..options()
        };
        assert!(
            load(dir.path(), temp.path(), &on_disk)
                .expect("ok")
                .is_none()
        );
        let elsewhere = temp.path().join("a");
        assert!(
            load(dir.path(), &elsewhere, &options())
                .expect("ok")
                .is_none()
        );
    }

    #[test]
    fn the_metric_is_applied_on_load() {
        let temp = fixture();
        let dir = TempDir::new().expect("cache dir");
        let tree = scan(temp.path(), options()).expect("scan");
        save(dir.path(), temp.path(), &options(), &tree, 1).expect("save");

        let by_files = ScanOptions {
            metric: crate::tree::Metric::Files,
            ..options()
        };
        let cached = load(dir.path(), temp.path(), &by_files)
            .expect("load")
            .expect("the same cache: the metric only orders");
        assert_eq!(&*cached.tree.children[0].name, "a", "two files beat one");
    }

    #[test]
    fn a_damaged_cache_is_an_error_not_a_tree() {
        let temp = fixture();
        let dir = TempDir::new().expect("cache dir");
        let tree = scan(temp.path(), options()).expect("scan");
        save(dir.path(), temp.path(), &options(), &tree, 1).expect("save");

        let path = dir.path().join(file_name(&key(temp.path(), &options())));
        let bytes = fs::read(&path).expect("read");
        fs::write(&path, &bytes[..bytes.len() / 2]).expect("truncate");
        assert!(load(dir.path(), temp.path(), &options()).is_err());

        fs::write(&path, b"not a cache at all").expect("garbage");
        assert!(matches!(
            load(dir.path(), temp.path(), &options()),
            Ok(None) | Err(_)
        ));
    }

    #[test]
    fn only_the_most_recent_caches_are_kept() {
        let dir = TempDir::new().expect("cache dir");
        let tree = Node::directory("root");
        for index in 0..KEEP + 3 {
            let root = PathBuf::from(format!("/root/{index}"));
            save(dir.path(), &root, &options(), &tree, 1).expect("save");
        }
        let count = fs::read_dir(dir.path()).expect("list").count();
        assert_eq!(count, KEEP);
    }

    #[test]
    fn varints_round_trip_at_the_edges() {
        for value in [0, 1, 127, 128, 16_383, 16_384, u64::MAX] {
            let mut bytes = Vec::new();
            Writer(&mut bytes).varint(value).expect("write");
            assert_eq!(Reader(&bytes[..]).varint().expect("read"), value);
        }
        for value in [0, -1, 1, i64::MIN, i64::MAX] {
            let mut bytes = Vec::new();
            Writer(&mut bytes).signed(value).expect("write");
            assert_eq!(Reader(&bytes[..]).signed().expect("read"), value);
        }
    }
}
