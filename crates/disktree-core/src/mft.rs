//! Reading an NTFS volume from its master file table.
//!
//! Every file and directory on NTFS is a fixed-size record in one table,
//! `$MFT`. Reading that table front to back is a handful of large
//! sequential reads, where walking the directories is one kernel round trip
//! per directory and most of the scan's time is spent in the kernel. It is
//! how `WizTree` and Everything are fast, and it is what this module does.
//!
//! It also answers two things a directory listing cannot on Windows without
//! opening every file: the space each file really occupies (allocated
//! clusters, so compressed, sparse and cloud-placeholder files count at what
//! they cost), and which names are hardlinks of one file (one record, many
//! names), which the ordinary de-duplication pass then charges once.
//!
//! The raw volume can only be opened by an administrator. Anything that
//! stops this path — no rights, not NTFS, a record it does not understand —
//! is an error the scanner answers by walking the directories instead, so
//! the file table is only ever a faster way to the same tree.
//!
//! Parsing works on byte slices with every offset checked, so it is plain
//! safe Rust and runs, and is tested, on any platform; only opening the
//! volume is Windows-specific.

use crate::scan::ScanOptions;
use crate::tree::{Node, NodeKind};

/// The update sequence protects each 512-byte stride of a record, whatever
/// the disk's sector size.
const STRIDE: usize = 512;
/// The root directory's record number, fixed by the format.
pub const ROOT_RECORD: u32 = 5;
/// Record references are 48 bits of record number over 16 bits of
/// sequence number.
const RECORD_MASK: u64 = 0x0000_ffff_ffff_ffff;
/// Deeper than any real volume nests; a corrupt table cannot recurse the
/// builder into a stack overflow.
const MAX_DEPTH: usize = 4096;

const ATTR_STANDARD_INFORMATION: u32 = 0x10;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_REPARSE_POINT: u32 = 0xc0;
const ATTR_END: u32 = 0xffff_ffff;

/// `$FILE_NAME` namespaces: the DOS 8.3 alias of a long name is a second
/// spelling of the same link, not a link of its own.
const NAMESPACE_DOS: u8 = 2;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
/// Reparse tags with this bit stand for another name — symlinks, junctions,
/// volumes mounted in a folder — and are links, not files.
const NAME_SURROGATE: u32 = 0x2000_0000;

/// Seconds from 1601, where Windows counts, to 1970.
const EPOCH_DIFFERENCE: i64 = 11_644_473_600;

/// Where the table is and how it is cut, from the boot sector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub cluster: u64,
    pub record: usize,
    /// Byte offset of the table's first record.
    pub mft_offset: u64,
}

/// Read the geometry from an NTFS boot sector; `None` for anything else.
pub fn parse_boot(sector: &[u8]) -> Option<Geometry> {
    if sector.get(3..11)? != b"NTFS    " {
        return None;
    }
    let bytes_per_sector = u64::from(read_u16(sector, 0x0b)?);
    if !bytes_per_sector.is_power_of_two()
        || !(256..=4096).contains(&bytes_per_sector)
    {
        return None;
    }
    // Cluster sizes past 64 KiB are stored as a negative power of two.
    let raw = *sector.get(0x0d)?;
    let sectors_per_cluster = if raw > 0x80 {
        1_u64.checked_shl(256 - u32::from(raw))?
    } else {
        u64::from(raw)
    };
    let cluster = bytes_per_sector.checked_mul(sectors_per_cluster)?;
    if cluster == 0 {
        return None;
    }
    let mft_cluster = read_u64(sector, 0x30)?;
    // Likewise a record: negative means 2^-n bytes, positive n clusters.
    let per_record = i8::from_ne_bytes([*sector.get(0x40)?]);
    let record = if per_record < 0 {
        1_u64.checked_shl(u32::from(per_record.unsigned_abs()))?
    } else {
        u64::try_from(per_record).ok()?.checked_mul(cluster)?
    };
    let record = usize::try_from(record).ok()?;
    if !(1024..=65_536).contains(&record) || !record.is_multiple_of(STRIDE) {
        return None;
    }
    Some(Geometry {
        cluster,
        record,
        mft_offset: mft_cluster.checked_mul(cluster)?,
    })
}

/// Undo the update sequence, or say the record is torn.
///
/// The last two bytes of every stride were swapped for a check value when
/// the record was written, and are put back here. A stride whose check
/// value is wrong was torn by a write in flight, and the record is not
/// trusted.
pub fn apply_fixups(record: &mut [u8]) -> bool {
    if record.len() < 48 || !record.len().is_multiple_of(STRIDE) {
        return false;
    }
    if record.get(0..4) != Some(b"FILE") {
        return false;
    }
    let (Some(offset), Some(count)) =
        (read_u16(record, 4), read_u16(record, 6))
    else {
        return false;
    };
    let (offset, count) = (usize::from(offset), usize::from(count));
    if count != record.len() / STRIDE + 1 || offset + count * 2 > record.len() {
        return false;
    }
    let check = [record[offset], record[offset + 1]];
    for stride in 1..count {
        let end = stride * STRIDE;
        if record[end - 2..end] != check {
            return false;
        }
        record[end - 2] = record[offset + stride * 2];
        record[end - 1] = record[offset + stride * 2 + 1];
    }
    true
}

/// One run of a non-resident attribute: `length` clusters at `start`, or a
/// sparse hole of `length` clusters when `start` is `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub start: Option<u64>,
    pub length: u64,
}

/// Decode a mapping-pairs array. Each run starts with a byte whose low
/// nibble is the width of its length and high nibble the width of its
/// start, which is signed and relative to the previous run's start.
pub fn decode_runs(bytes: &[u8]) -> Option<Vec<Run>> {
    let mut runs = Vec::new();
    let mut position = 0;
    let mut start = 0_i64;
    loop {
        let header = *bytes.get(position)?;
        if header == 0 {
            return Some(runs);
        }
        let length_width = usize::from(header & 0x0f);
        let start_width = usize::from(header >> 4);
        if length_width == 0 || length_width > 8 || start_width > 8 {
            return None;
        }
        position += 1;
        let length = unsigned(bytes.get(position..position + length_width)?);
        position += length_width;
        if start_width == 0 {
            runs.push(Run {
                start: None,
                length,
            });
            continue;
        }
        let delta = signed(bytes.get(position..position + start_width)?);
        position += start_width;
        start = start.checked_add(delta)?;
        runs.push(Run {
            start: Some(u64::try_from(start).ok()?),
            length,
        });
    }
}

/// What one record says, before extension records are merged into their
/// base.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    /// The base record this extends, or `None` for a base record.
    pub base: Option<u32>,
    pub is_dir: bool,
    /// Every link: the parent directory's record and the name in it.
    pub names: Vec<(u32, Box<str>)>,
    /// Last write, Unix seconds.
    pub modified: Option<i64>,
    /// Bytes the file's streams occupy on disk.
    pub allocated: u64,
    /// Length of the unnamed stream: what `dir` shows.
    pub apparent: u64,
    pub reparse_tag: Option<u32>,
    /// Where the reparse data starts when it did not fit in the record, so
    /// its tag can be read from there.
    pub reparse_cluster: Option<u64>,
}

/// Parse one record in place (fixups are applied to `bytes`). `None` for a
/// record that is free, torn or not a record. `cluster` turns runs into
/// bytes.
pub fn parse_record(bytes: &mut [u8], cluster: u64) -> Option<Record> {
    if !apply_fixups(bytes) {
        return None;
    }
    let flags = read_u16(bytes, 0x16)?;
    if flags & 0x01 == 0 {
        return None;
    }
    let base = read_u64(bytes, 0x20)? & RECORD_MASK;
    let mut record = Record {
        base: (base != 0).then_some(u32::try_from(base).ok()?),
        is_dir: flags & 0x02 != 0,
        ..Record::default()
    };
    let used = usize::try_from(read_u32(bytes, 0x18)?)
        .ok()?
        .min(bytes.len());
    let mut position = usize::from(read_u16(bytes, 0x14)?);
    let mut reparse_from_name = None;
    while position + 8 <= used {
        let kind = read_u32(bytes, position)?;
        if kind == ATTR_END {
            break;
        }
        let length = usize::try_from(read_u32(bytes, position + 4)?).ok()?;
        if length < 16 || position + length > used {
            return None;
        }
        let attribute = &bytes[position..position + length];
        position += length;
        let resident = attribute[8] == 0;
        let named = attribute[9] != 0;
        match kind {
            ATTR_STANDARD_INFORMATION if resident => {
                let value = resident_value(attribute)?;
                record.modified = read_u64(value, 0x08).map(filetime_to_unix);
            }
            ATTR_FILE_NAME if resident => {
                let value = resident_value(attribute)?;
                let parent = read_u64(value, 0)? & RECORD_MASK;
                let length = usize::from(*value.get(0x40)?);
                let namespace = *value.get(0x41)?;
                if read_u32(value, 0x38)? & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    reparse_from_name = read_u32(value, 0x3c);
                }
                if namespace != NAMESPACE_DOS {
                    let name = utf16(value.get(0x42..0x42 + length * 2)?);
                    record
                        .names
                        .push((u32::try_from(parent).ok()?, name.into()));
                }
            }
            ATTR_DATA if resident => {
                if !named {
                    record.apparent += u64::from(read_u32(attribute, 0x10)?);
                }
            }
            ATTR_DATA => {
                // What a stream occupies is its runs that are not holes.
                // The header's allocated size counts holes whenever the
                // stream is not flagged sparse — `$BadClus` claims the
                // whole volume that way — so the runs are the truth, and
                // every piece of a fragmented stream brings its own.
                let first = read_u64(attribute, 0x10)? == 0;
                let runs = attribute
                    .get(usize::from(read_u16(attribute, 0x20)?)..)
                    .and_then(decode_runs);
                match runs {
                    Some(runs) => {
                        let clusters: u64 = runs
                            .iter()
                            .filter(|run| run.start.is_some())
                            .map(|run| run.length)
                            .sum();
                        record.allocated += clusters.saturating_mul(cluster);
                    }
                    None if first => {
                        record.allocated += read_u64(attribute, 0x28)?;
                    }
                    None => {}
                }
                // Only the piece starting at cluster zero carries lengths.
                if first && !named {
                    record.apparent += read_u64(attribute, 0x30)?;
                }
            }
            ATTR_REPARSE_POINT if resident => {
                record.reparse_tag = read_u32(resident_value(attribute)?, 0);
            }
            // A long link target pushes the data out of the record; the tag
            // is its first four bytes, at the first run.
            ATTR_REPARSE_POINT if read_u64(attribute, 0x10)? == 0 => {
                let runs = decode_runs(
                    attribute.get(usize::from(read_u16(attribute, 0x20)?)..)?,
                )?;
                record.reparse_cluster = runs.first().and_then(|run| run.start);
            }
            _ => {}
        }
    }
    if record.reparse_tag.is_none() {
        record.reparse_tag = reparse_from_name;
    }
    Some(record)
}

/// A stream's bytes: inline in the record, or runs on the volume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stream {
    Resident(Vec<u8>),
    NonResident { runs: Vec<Run>, size: u64 },
}

/// Where `$MFT` lives, from its own record 0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extent {
    /// The table: runs and length in bytes.
    pub runs: Vec<Run>,
    pub size: u64,
    /// One bit per record, set when it is in use; `None` if record 0 does
    /// not carry it, and then every record is read.
    pub bitmap: Option<Stream>,
}

const ATTR_BITMAP: u32 = 0xb0;

/// Read where the table and its in-use bitmap are. `None` if the table's
/// runs are not all in record 0, which only a very fragmented table does;
/// the directory walk handles that volume instead.
pub fn table_extent(record_zero: &mut [u8]) -> Option<Extent> {
    if !apply_fixups(record_zero) {
        return None;
    }
    let used = usize::try_from(read_u32(record_zero, 0x18)?)
        .ok()?
        .min(record_zero.len());
    let mut position = usize::from(read_u16(record_zero, 0x14)?);
    let mut table = None;
    let mut bitmap = None;
    while position + 8 <= used {
        let kind = read_u32(record_zero, position)?;
        if kind == ATTR_END {
            break;
        }
        let length =
            usize::try_from(read_u32(record_zero, position + 4)?).ok()?;
        if length < 16 || position + length > used {
            return None;
        }
        let attribute = &record_zero[position..position + length];
        position += length;
        if attribute[9] != 0 || (kind != ATTR_DATA && kind != ATTR_BITMAP) {
            continue;
        }
        let stream = if attribute[8] == 0 {
            Stream::Resident(resident_value(attribute)?.to_vec())
        } else {
            if read_u64(attribute, 0x10)? != 0 {
                continue;
            }
            let last_vcn = read_u64(attribute, 0x18)?;
            let runs = decode_runs(
                attribute.get(usize::from(read_u16(attribute, 0x20)?)..)?,
            )?;
            // All of it must be mapped here, or it continues in an
            // extension record this reader does not chase.
            let mapped: u64 = runs.iter().map(|run| run.length).sum();
            if mapped != last_vcn + 1 {
                if kind == ATTR_DATA {
                    return None;
                }
                continue;
            }
            Stream::NonResident {
                runs,
                size: read_u64(attribute, 0x30)?,
            }
        };
        if kind == ATTR_DATA {
            table = Some(stream);
        } else {
            bitmap = Some(stream);
        }
    }
    match table? {
        Stream::NonResident { runs, size } => {
            Some(Extent { runs, size, bitmap })
        }
        // A table small enough to sit in its own record is not a volume.
        Stream::Resident(_) => None,
    }
}

/// A stretch of the volume that holds part of a piece; `None` is a hole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub offset: Option<u64>,
    pub length: usize,
}

/// Whole clusters of the table read as one unit of work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    pub first_record: u64,
    /// Bytes read: always whole clusters, as a raw volume requires.
    pub length: usize,
    /// Records in it that belong to the table; the table's last cluster
    /// may be only partly used.
    pub records: usize,
    pub segments: Vec<Segment>,
}

/// Stretches this small are checked against the in-use bitmap on their
/// own, so a free stretch this long is not read at all.
const GRANULE: usize = 64 << 10;

/// Cut the table into pieces to read, leaving out what is free.
///
/// The table is `size` bytes laid out over `runs`; pieces are about `chunk`
/// bytes, and stretches whose records are all free per `in_use` (the
/// table's own `$BITMAP`, one bit per record) are left out. Every piece is
/// whole clusters and whole records, so every read is aligned for the raw
/// volume; a piece that crosses runs or skips a free stretch is read as
/// several segments.
pub fn plan_pieces(
    runs: &[Run],
    geometry: Geometry,
    size: u64,
    chunk: usize,
    in_use: Option<&[u8]>,
) -> Vec<Piece> {
    let used = |piece: &Piece| {
        let in_bitmap = |number: u64| {
            in_use.is_none_or(|bits| {
                let byte = usize::try_from(number / 8).unwrap_or(usize::MAX);
                // Past the bitmap's end, read rather than guess.
                bits.get(byte)
                    .is_none_or(|bits| bits >> (number % 8) & 1 != 0)
            })
        };
        piece
            .segments
            .iter()
            .any(|segment| segment.offset.is_some())
            && (piece.first_record..piece.first_record + piece.records as u64)
                .any(in_bitmap)
    };
    let mut pieces: Vec<Piece> = Vec::new();
    for granule in cut(runs, geometry, size, GRANULE) {
        if !used(&granule) {
            continue;
        }
        match pieces.last_mut() {
            Some(piece)
                if piece.first_record + piece.records as u64
                    == granule.first_record
                    && piece.records * geometry.record == piece.length
                    && piece.length + granule.length <= chunk =>
            {
                for segment in granule.segments {
                    match piece.segments.last_mut() {
                        Some(last)
                            if last.offset.zip(segment.offset).is_some_and(
                                |(last_start, start)| {
                                    last_start + last.length as u64 == start
                                },
                            ) =>
                        {
                            last.length += segment.length;
                        }
                        _ => piece.segments.push(segment),
                    }
                }
                piece.length += granule.length;
                piece.records += granule.records;
            }
            _ => pieces.push(granule),
        }
    }
    pieces
}

/// Cut the table into consecutive pieces of `chunk` bytes, rounded to
/// whole clusters and records.
fn cut(
    runs: &[Run],
    geometry: Geometry,
    size: u64,
    chunk: usize,
) -> Vec<Piece> {
    let cluster = geometry.cluster;
    let record = geometry.record as u64;
    let total_records = size / record;
    // A multiple of both, so pieces end on record and cluster boundaries.
    let step = cluster.max(record);
    let chunk = (chunk as u64 / step).max(1) * step;
    let empty = |first_record| Piece {
        first_record,
        length: 0,
        records: 0,
        segments: Vec::new(),
    };
    let mut pieces = Vec::new();
    let mut current = empty(0);
    let mut remaining = size.div_ceil(cluster) * cluster;
    for run in runs {
        let mut bytes = (run.length * cluster).min(remaining);
        let mut offset = run.start.map(|start| start * cluster);
        while bytes > 0 {
            let take = bytes.min(chunk - current.length as u64);
            current.segments.push(Segment {
                offset,
                length: take as usize,
            });
            current.length += take as usize;
            offset = offset.map(|offset| offset + take);
            bytes -= take;
            remaining -= take;
            if current.length as u64 == chunk {
                let next = current.first_record + chunk / record;
                pieces.push(std::mem::replace(&mut current, empty(next)));
            }
        }
    }
    if current.length > 0 {
        pieces.push(current);
    }
    for piece in &mut pieces {
        let fits = piece.length as u64 / record;
        let left = total_records.saturating_sub(piece.first_record);
        piece.records = fits.min(left) as usize;
    }
    pieces
}

/// A file or directory once its extension records are folded in.
#[derive(Clone, Debug, Default)]
struct Entry {
    in_use: bool,
    is_dir: bool,
    /// Links: several means hardlinks, charged once.
    links: u16,
    modified: i64,
    allocated: u64,
    apparent: u64,
    reparse_tag: Option<u32>,
    reparse_cluster: Option<u64>,
}

impl Entry {
    fn is_link(&self) -> bool {
        self.reparse_tag
            .is_some_and(|tag| tag & NAME_SURROGATE != 0)
    }
}

/// The whole table, merged: every entry by record number, and every name
/// under its parent.
#[derive(Debug, Default)]
pub struct Table {
    entries: Vec<Entry>,
    /// `(parent, child, name)` for every link, gathered as records land.
    links: Vec<(u32, u32, Box<str>)>,
}

impl Table {
    /// Add one parsed record, numbered `number`.
    pub fn add(&mut self, number: u32, record: Record) {
        let owner = record.base.unwrap_or(number);
        let index = owner as usize;
        if self.entries.len() <= index {
            self.entries.resize_with(index + 1, Entry::default);
        }
        let entry = &mut self.entries[index];
        if record.base.is_none() {
            entry.in_use = true;
            entry.is_dir = record.is_dir;
            entry.modified = record.modified.unwrap_or(0);
        }
        entry.allocated += record.allocated;
        entry.apparent += record.apparent;
        if record.reparse_tag.is_some() {
            entry.reparse_tag = record.reparse_tag;
        }
        if record.reparse_cluster.is_some() {
            entry.reparse_cluster = record.reparse_cluster;
        }
        for (parent, name) in record.names {
            entry.links = entry.links.saturating_add(1);
            // The root lists itself as its own parent.
            if parent != owner {
                self.links.push((parent, owner, name));
            }
        }
    }

    /// Entries whose reparse tag lives outside the record: the entry and
    /// the cluster to read it from.
    pub fn unresolved_reparse(&self) -> Vec<(u32, u64)> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.in_use && entry.reparse_tag.is_none())
            .filter_map(|(number, entry)| {
                Some((u32::try_from(number).ok()?, entry.reparse_cluster?))
            })
            .collect()
    }

    pub fn set_reparse_tag(&mut self, number: u32, tag: u32) {
        if let Some(entry) = self.entries.get_mut(number as usize) {
            entry.reparse_tag = Some(tag);
        }
    }

    /// Index the links by parent, ready to build trees from.
    pub fn index(self) -> Index {
        let Self { entries, mut links } = self;
        links.retain(|(parent, child, _)| {
            let live = |number: u32| {
                entries
                    .get(number as usize)
                    .is_some_and(|entry| entry.in_use)
            };
            live(*parent) && live(*child)
        });
        links.sort_unstable_by_key(|(parent, _, _)| *parent);
        let mut offsets = vec![0_usize; entries.len() + 1];
        for (parent, _, _) in &links {
            offsets[*parent as usize + 1] += 1;
        }
        for index in 1..offsets.len() {
            offsets[index] += offsets[index - 1];
        }
        let children = links
            .into_iter()
            .map(|(_, child, name)| (child, name))
            .collect();
        Index {
            entries,
            offsets,
            children,
        }
    }
}

/// The table as a tree: children of each directory, by record number.
#[derive(Debug)]
pub struct Index {
    entries: Vec<Entry>,
    offsets: Vec<usize>,
    children: Vec<(u32, Box<str>)>,
}

impl Index {
    fn children(&self, parent: u32) -> &[(u32, Box<str>)] {
        let parent = parent as usize;
        match (self.offsets.get(parent), self.offsets.get(parent + 1)) {
            (Some(&start), Some(&end)) => &self.children[start..end],
            _ => &[],
        }
    }

    /// The record of the directory at `components` below the root, matched
    /// without regard to case, as Windows resolves a path.
    pub fn resolve<'a>(
        &self,
        components: impl IntoIterator<Item = &'a str>,
    ) -> Option<u32> {
        let mut current = ROOT_RECORD;
        for component in components {
            let children = self.children(current);
            current = children
                .iter()
                .find(|(_, name)| name.as_ref() == component)
                .or_else(|| {
                    let wanted = component.to_lowercase();
                    children
                        .iter()
                        .find(|(_, name)| name.to_lowercase() == wanted)
                })
                .map(|(child, _)| *child)?;
        }
        Some(current)
    }

    /// Build the tree below `root`, named `name`, as the directory walk
    /// would: same hidden rule, same depth limit, links not entered.
    /// Totals are left to [`crate::scan`]'s finishing pass.
    pub fn build(&self, root: u32, name: &str, options: &ScanOptions) -> Node {
        let mut entered = vec![false; self.entries.len()];
        self.node(root, name.into(), 0, options, &mut entered)
    }

    fn node(
        &self,
        number: u32,
        name: Box<str>,
        depth: usize,
        options: &ScanOptions,
        entered: &mut [bool],
    ) -> Node {
        let entry = &self.entries[number as usize];
        if entry.is_dir && !entry.is_link() {
            let mut node = Node::directory(name);
            // A directory has one parent; seeing one twice is a corrupt
            // table, and entering it again could never end.
            let first_visit =
                !std::mem::replace(&mut entered[number as usize], true);
            let descend = first_visit
                && depth < MAX_DEPTH
                && options.max_depth.is_none_or(|limit| depth < limit);
            if descend {
                for (child, child_name) in self.children(number) {
                    if !options.include_hidden && child_name.starts_with('.') {
                        continue;
                    }
                    node.children.push(self.node(
                        *child,
                        child_name.clone(),
                        depth + 1,
                        options,
                        entered,
                    ));
                }
            }
            return node;
        }
        let kind = if entry.is_link() {
            NodeKind::Symlink
        } else if entry.is_dir {
            NodeKind::Other
        } else {
            NodeKind::File
        };
        let bytes = if options.apparent_size {
            entry.apparent
        } else {
            entry.allocated
        };
        let mut node = Node::entry(name, kind, bytes);
        node.modified = entry.modified;
        // One record under several names: the finishing pass charges it
        // once. The device half of the key is unused, as this is one volume.
        if entry.links > 1 {
            node.inode = Some((u64::MAX, u64::from(number)));
        }
        node
    }
}

fn resident_value(attribute: &[u8]) -> Option<&[u8]> {
    let length = usize::try_from(read_u32(attribute, 0x10)?).ok()?;
    let offset = usize::from(read_u16(attribute, 0x14)?);
    attribute.get(offset..offset.checked_add(length)?)
}

fn filetime_to_unix(filetime: u64) -> i64 {
    i64::try_from(filetime / 10_000_000)
        .map_or(0, |seconds| seconds - EPOCH_DIFFERENCE)
}

fn utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

fn unsigned(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .rev()
        .fold(0, |value, byte| (value << 8) | u64::from(*byte))
}

fn signed(bytes: &[u8]) -> i64 {
    let value = unsigned(bytes);
    let bits = bytes.len() * 8;
    // Sign-extend from the top bit of the last byte.
    if bits < 64 && value & (1 << (bits - 1)) != 0 {
        (value | (u64::MAX << bits)).cast_signed()
    } else {
        value.cast_signed()
    }
}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn read_u64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

#[cfg(windows)]
pub use volume::scan;

/// Opening and reading the table off a live volume.
#[cfg(windows)]
mod volume {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::fs::{FileExt as _, OpenOptionsExt as _};
    use std::path::{Component, Path, Prefix};

    use rayon::prelude::*;

    use super::{Geometry, Piece, Record, Run, Stream, Table};
    use crate::scan::{ScanOptions, ScanProgress};
    use crate::tree::Node;

    /// Pieces are about this long: one long transfer each, and enough of
    /// them in flight to keep the drive busy.
    const CHUNK: usize = 8 << 20;
    /// Read around the file cache: the table is read once, and pushing
    /// gigabytes of it through the cache would evict what the rest of the
    /// machine is using. Requires sector-aligned buffers; see [`Aligned`].
    const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
    /// No sector is larger, so this alignment satisfies every drive.
    const ALIGNMENT: usize = 4096;

    /// Build the tree below `root` from its volume's file table.
    pub fn scan(
        root: &Path,
        options: &ScanOptions,
        progress: &ScanProgress,
    ) -> io::Result<Node> {
        let (letter, components) = split(root)?;
        let volume = open(letter)?;
        let boot = read_at(&volume, 0, ALIGNMENT)?;
        let geometry = super::parse_boot(&boot)
            .ok_or_else(|| unsupported("not an NTFS volume"))?;
        let first = read_at(
            &volume,
            geometry.mft_offset,
            geometry.record.max(geometry.cluster as usize),
        )?;
        let mut zero = first[..geometry.record].to_vec();
        let extent = super::table_extent(&mut zero)
            .ok_or_else(|| unsupported("the file table is too fragmented"))?;
        // Without the bitmap every record is read; slower, not wrong.
        let bitmap = extent
            .bitmap
            .as_ref()
            .and_then(|stream| read_stream(&volume, geometry, stream).ok());

        let pieces = super::plan_pieces(
            &extent.runs,
            geometry,
            extent.size,
            CHUNK,
            bitmap.as_deref(),
        );
        let mut table = read_table(&volume, geometry, &pieces, progress)?;
        resolve_reparse_tags(&volume, geometry, &mut table);
        let index = table.index();
        let record = index
            .resolve(components.iter().map(String::as_str))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "the root is not in the file table",
                )
            })?;
        Ok(index.build(record, &crate::scan::root_name(root), options))
    }

    /// Open the volume for reading, having flushed it first where allowed.
    ///
    /// The table is read from the disk, but what was written a moment ago
    /// may still be only in the file cache: a removal would still show until
    /// the lazy writer caught up. Flushing a volume handle writes every
    /// cached change out, which is the documented way to do that; it needs
    /// a handle opened for writing, which is closed again at once and never
    /// written through. Where the flush is refused the read goes ahead, a
    /// few seconds behind at worst.
    fn open(letter: char) -> io::Result<File> {
        let device = format!(r"\\.\{letter}:");
        if let Ok(writable) =
            OpenOptions::new().read(true).write(true).open(&device)
        {
            let _ = writable.sync_all();
        }
        OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_NO_BUFFERING)
            .open(&device)
    }

    /// The drive letter, and the directory names below its root.
    fn split(root: &Path) -> io::Result<(char, Vec<String>)> {
        let mut components = root.components();
        let letter = match components.next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                    char::from(letter)
                }
                _ => return Err(unsupported("not a local drive")),
            },
            _ => return Err(unsupported("not a drive path")),
        };
        let rest = components
            .filter_map(|component| match component {
                Component::Normal(name) => {
                    Some(name.to_string_lossy().into_owned())
                }
                _ => None,
            })
            .collect();
        Ok((letter, rest))
    }

    /// Read and parse the pieces, many at once: each worker reads its own
    /// piece with a positional read on the shared handle and parses it, so
    /// the drive always has several requests queued. Records are merged in
    /// order afterwards, which is cheap next to reading them.
    fn read_table(
        volume: &File,
        geometry: Geometry,
        pieces: &[Piece],
        progress: &ScanProgress,
    ) -> io::Result<Table> {
        let parsed: Vec<Vec<(u64, Record)>> = pieces
            .par_iter()
            .map(|piece| {
                if progress.is_cancelled() {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let mut buffer = Aligned::new(piece.length);
                let data = buffer.as_mut();
                let mut at = 0;
                for segment in &piece.segments {
                    let target = &mut data[at..at + segment.length];
                    // A hole is left as zeros, which no record starts with.
                    if let Some(offset) = segment.offset {
                        read_exact_at(volume, target, offset)?;
                    }
                    at += segment.length;
                }
                let records: Vec<(u64, Record)> = data
                    .chunks_exact_mut(geometry.record)
                    .take(piece.records)
                    .enumerate()
                    .filter_map(|(index, bytes)| {
                        let number = piece.first_record + index as u64;
                        super::parse_record(bytes, geometry.cluster)
                            .map(|parsed| (number, parsed))
                    })
                    .collect();
                for (_, parsed) in &records {
                    if parsed.base.is_none() {
                        if parsed.is_dir {
                            progress.count_dir();
                        } else {
                            progress.count_file(parsed.allocated);
                        }
                    }
                }
                Ok(records)
            })
            .collect::<io::Result<_>>()?;

        let mut table = Table::default();
        for (number, record) in parsed.into_iter().flatten() {
            if let Ok(number) = u32::try_from(number) {
                table.add(number, record);
            }
        }
        Ok(table)
    }

    /// Find the tags of links whose reparse data did not fit in their
    /// record: one small read each, for the few that need it. pnpm's
    /// junctions, with their long targets, are the common case. A tag that
    /// cannot be read leaves the entry a directory with nothing in it.
    fn resolve_reparse_tags(
        volume: &File,
        geometry: Geometry,
        table: &mut Table,
    ) {
        let tags: Vec<(u32, Option<u32>)> = table
            .unresolved_reparse()
            .par_iter()
            .map(|&(entry, cluster)| {
                let tag = cluster
                    .checked_mul(geometry.cluster)
                    .and_then(|offset| {
                        read_at(volume, offset, geometry.cluster as usize).ok()
                    })
                    .and_then(|data| super::read_u32(&data, 0));
                (entry, tag)
            })
            .collect();
        for (entry, tag) in tags {
            if let Some(tag) = tag {
                table.set_reparse_tag(entry, tag);
            }
        }
    }

    /// A small stream's bytes, such as the in-use bitmap.
    fn read_stream(
        volume: &File,
        geometry: Geometry,
        stream: &Stream,
    ) -> io::Result<Vec<u8>> {
        let (runs, size): (&[Run], u64) = match stream {
            Stream::Resident(bytes) => return Ok(bytes.clone()),
            Stream::NonResident { runs, size } => (runs, *size),
        };
        let mut out = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
        for run in runs {
            let length = usize::try_from(run.length * geometry.cluster)
                .map_err(|_| unsupported("a bitmap larger than memory"))?;
            match run.start {
                Some(start) => out.extend_from_slice(&read_at(
                    volume,
                    start * geometry.cluster,
                    length,
                )?),
                None => out.resize(out.len() + length, 0),
            }
            if out.len() as u64 >= size {
                break;
            }
        }
        out.truncate(usize::try_from(size).unwrap_or(out.len()));
        Ok(out)
    }

    /// A buffer whose start is aligned for unbuffered reads, carved out of
    /// an ordinary allocation so no unsafe code is needed.
    struct Aligned {
        bytes: Vec<u8>,
        start: usize,
        length: usize,
    }

    impl Aligned {
        fn new(length: usize) -> Self {
            let bytes = vec![0_u8; length + ALIGNMENT];
            let start = bytes.as_ptr().align_offset(ALIGNMENT);
            Self {
                bytes,
                start,
                length,
            }
        }

        fn as_mut(&mut self) -> &mut [u8] {
            &mut self.bytes[self.start..self.start + self.length]
        }
    }

    /// Read `length` bytes at `offset`; both must be whole sectors.
    fn read_at(
        volume: &File,
        offset: u64,
        length: usize,
    ) -> io::Result<Vec<u8>> {
        let mut buffer = Aligned::new(length);
        read_exact_at(volume, buffer.as_mut(), offset)?;
        Ok(buffer.as_mut().to_vec())
    }

    fn read_exact_at(
        volume: &File,
        mut buffer: &mut [u8],
        mut offset: u64,
    ) -> io::Result<()> {
        while !buffer.is_empty() {
            match volume.seek_read(buffer, offset) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(read) => {
                    buffer = &mut buffer[read..];
                    offset += read as u64;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn unsupported(why: &str) -> io::Error {
        io::Error::new(io::ErrorKind::Unsupported, why.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record builder: header, attributes, fixups — the same bytes NTFS
    /// writes, so the parser is tested against the format, not itself.
    struct Builder {
        bytes: Vec<u8>,
        position: usize,
    }

    impl Builder {
        fn new(flags: u16, base: u64) -> Self {
            let mut bytes = vec![0_u8; 1024];
            bytes[0..4].copy_from_slice(b"FILE");
            bytes[4..6].copy_from_slice(&0x30_u16.to_le_bytes());
            bytes[6..8].copy_from_slice(&3_u16.to_le_bytes());
            bytes[0x14..0x16].copy_from_slice(&0x38_u16.to_le_bytes());
            bytes[0x16..0x18].copy_from_slice(&flags.to_le_bytes());
            bytes[0x1c..0x20].copy_from_slice(&1024_u32.to_le_bytes());
            bytes[0x20..0x28].copy_from_slice(&base.to_le_bytes());
            Self {
                bytes,
                position: 0x38,
            }
        }

        fn attribute(mut self, header: &[u8]) -> Self {
            let end = self.position + header.len();
            self.bytes[self.position..end].copy_from_slice(header);
            self.position = end;
            self
        }

        fn resident(self, kind: u32, name: &str, value: &[u8]) -> Self {
            let name: Vec<u8> =
                name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let value_offset = (0x18 + name.len()).next_multiple_of(8);
            let length = (value_offset + value.len()).next_multiple_of(8);
            let mut header = vec![0_u8; length];
            header[0..4].copy_from_slice(&kind.to_le_bytes());
            header[4..8].copy_from_slice(&(length as u32).to_le_bytes());
            header[9] = (name.len() / 2) as u8;
            header[0x0a..0x0c].copy_from_slice(&0x18_u16.to_le_bytes());
            header[0x10..0x14]
                .copy_from_slice(&(value.len() as u32).to_le_bytes());
            header[0x14..0x16]
                .copy_from_slice(&(value_offset as u16).to_le_bytes());
            header[0x18..0x18 + name.len()].copy_from_slice(&name);
            header[value_offset..value_offset + value.len()]
                .copy_from_slice(value);
            self.attribute(&header)
        }

        fn non_resident(
            self,
            name: &str,
            flags: u16,
            runs: &[u8],
            sizes: (u64, u64, u64),
        ) -> Self {
            let (allocated, data, total) = sizes;
            let name: Vec<u8> =
                name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let runs_offset = (0x48 + name.len()).next_multiple_of(8);
            let length = (runs_offset + runs.len() + 1).next_multiple_of(8);
            let mut header = vec![0_u8; length];
            header[0..4].copy_from_slice(&ATTR_DATA.to_le_bytes());
            header[4..8].copy_from_slice(&(length as u32).to_le_bytes());
            header[8] = 1;
            header[9] = (name.len() / 2) as u8;
            header[0x0a..0x0c].copy_from_slice(&0x48_u16.to_le_bytes());
            header[0x0c..0x0e].copy_from_slice(&flags.to_le_bytes());
            let clusters: u64 = decode_runs(&[runs, &[0]].concat())
                .expect("runs")
                .iter()
                .map(|run| run.length)
                .sum();
            header[0x18..0x20]
                .copy_from_slice(&(clusters.max(1) - 1).to_le_bytes());
            header[0x20..0x22]
                .copy_from_slice(&(runs_offset as u16).to_le_bytes());
            header[0x28..0x30].copy_from_slice(&allocated.to_le_bytes());
            header[0x30..0x38].copy_from_slice(&data.to_le_bytes());
            header[0x40..0x48].copy_from_slice(&total.to_le_bytes());
            header[0x48..0x48 + name.len()].copy_from_slice(&name);
            header[runs_offset..runs_offset + runs.len()].copy_from_slice(runs);
            self.attribute(&header)
        }

        fn standard(self, modified_unix: i64) -> Self {
            let filetime =
                ((modified_unix + EPOCH_DIFFERENCE) * 10_000_000) as u64;
            let mut value = vec![0_u8; 0x48];
            value[0x08..0x10].copy_from_slice(&filetime.to_le_bytes());
            self.resident(ATTR_STANDARD_INFORMATION, "", &value)
        }

        fn file_name(self, parent: u64, name: &str, namespace: u8) -> Self {
            self.file_name_with(parent, name, namespace, 0, 0)
        }

        fn file_name_with(
            self,
            parent: u64,
            name: &str,
            namespace: u8,
            attributes: u32,
            reparse: u32,
        ) -> Self {
            let units: Vec<u8> =
                name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let mut value = vec![0_u8; 0x42 + units.len()];
            // A sequence number in the high bits, as a real reference has.
            let reference = parent | (7 << 48);
            value[0..8].copy_from_slice(&reference.to_le_bytes());
            value[0x38..0x3c].copy_from_slice(&attributes.to_le_bytes());
            value[0x3c..0x40].copy_from_slice(&reparse.to_le_bytes());
            value[0x40] = (units.len() / 2) as u8;
            value[0x41] = namespace;
            value[0x42..].copy_from_slice(&units);
            self.resident(ATTR_FILE_NAME, "", &value)
        }

        /// End the attribute list and protect the record with fixups.
        fn finish(mut self) -> Vec<u8> {
            let end = self.position;
            self.bytes[end..end + 4].copy_from_slice(&ATTR_END.to_le_bytes());
            let used = (end + 8) as u32;
            self.bytes[0x18..0x1c].copy_from_slice(&used.to_le_bytes());
            let check = [0xab, 0xcd];
            self.bytes[0x30..0x32].copy_from_slice(&check);
            for stride in 1..=2 {
                let tail = stride * STRIDE - 2;
                let saved = [self.bytes[tail], self.bytes[tail + 1]];
                self.bytes[0x30 + stride * 2..0x32 + stride * 2]
                    .copy_from_slice(&saved);
                self.bytes[tail..tail + 2].copy_from_slice(&check);
            }
            self.bytes
        }
    }

    const DIRECTORY: u16 = 0x03;
    const FILE: u16 = 0x01;

    fn directory(parent: u64, name: &str) -> Vec<u8> {
        Builder::new(DIRECTORY, 0)
            .standard(1_000)
            .file_name(parent, name, 1)
            .finish()
    }

    const SPARSE: u16 = 0x8000;

    /// One run of `clusters` at cluster 0x20, as mapping pairs.
    fn run(clusters: u64) -> Vec<u8> {
        let [low, high, ..] = clusters.to_le_bytes();
        vec![0x12, low, high, 0x20]
    }

    fn file(parent: u64, name: &str, allocated: u64, apparent: u64) -> Vec<u8> {
        Builder::new(FILE, 0)
            .standard(2_000)
            .file_name(parent, name, 3)
            .non_resident(
                "",
                0,
                &run(allocated / 4096),
                (allocated, apparent, 0),
            )
            .finish()
    }

    #[test]
    fn holes_are_not_space_even_when_the_header_says_so() {
        // `$BadClus:$Bad`: as long as the volume, all hole, not flagged
        // sparse, and its header's allocated size is the whole volume.
        let hole = [0x03, 0xa0, 0x86, 0x01]; // 100,000 clusters, no start
        let record = parse(
            Builder::new(FILE, 0)
                .file_name(5, "$BadClus", 1)
                .non_resident(
                    "$Bad",
                    0,
                    &hole,
                    (100_000 * 4096, 100_000 * 4096, 0),
                )
                .finish(),
        );
        assert_eq!(record.allocated, 0);
    }

    fn parse(mut bytes: Vec<u8>) -> Record {
        parse_record(&mut bytes, 4096).expect("a record")
    }

    /// A little volume: `/` (5) holds `Users` (40) and `$MFT` (0); Users
    /// holds `tobi` (41), which holds a file, a hidden directory, a
    /// junction and a hardlinked file under two names.
    fn table() -> Table {
        let mut table = Table::default();
        let records: Vec<(u32, Vec<u8>)> = vec![
            (0, file(5, "$MFT", 1 << 20, 1 << 20)),
            (5, directory(5, ".")),
            (40, directory(5, "Users")),
            (41, directory(40, "tobi")),
            (42, file(41, "notes.txt", 4096, 1000)),
            (43, directory(41, ".cache")),
            (44, file(43, "blob.bin", 65_536, 65_000)),
            (
                45,
                Builder::new(DIRECTORY, 0)
                    .standard(3_000)
                    .file_name_with(
                        41,
                        "Application Data",
                        1,
                        0x400,
                        0xa000_0003,
                    )
                    .finish(),
            ),
            (
                46,
                Builder::new(FILE, 0)
                    .standard(4_000)
                    .file_name(41, "one.bin", 1)
                    .file_name(43, "same.bin", 1)
                    .file_name(41, "ONE~1.BIN", NAMESPACE_DOS)
                    .non_resident("", 0, &[0x11, 0x02, 0x30], (8192, 8000, 0))
                    .finish(),
            ),
        ];
        for (number, bytes) in records {
            table.add(number, parse(bytes));
        }
        table
    }

    fn options() -> ScanOptions {
        ScanOptions::default()
    }

    fn named<'a>(node: &'a Node, name: &str) -> &'a Node {
        node.child_named(name)
            .unwrap_or_else(|| panic!("{name} in {}", node.name))
    }

    #[test]
    fn a_boot_sector_gives_the_geometry() {
        let mut sector = vec![0_u8; 512];
        sector[3..11].copy_from_slice(b"NTFS    ");
        sector[0x0b..0x0d].copy_from_slice(&512_u16.to_le_bytes());
        sector[0x0d] = 8;
        sector[0x30..0x38].copy_from_slice(&786_432_u64.to_le_bytes());
        sector[0x40] = 0xf6; // -10: 2^10-byte records
        assert_eq!(
            parse_boot(&sector),
            Some(Geometry {
                cluster: 4096,
                record: 1024,
                mft_offset: 786_432 * 4096,
            })
        );
        sector[3..11].copy_from_slice(b"EXFAT   ");
        assert_eq!(parse_boot(&sector), None, "only NTFS");
    }

    #[test]
    fn fixups_restore_each_stride_and_catch_a_torn_write() {
        let bytes = directory(5, "Users");
        let mut good = bytes.clone();
        assert!(apply_fixups(&mut good));
        assert_eq!(&good[STRIDE - 2..STRIDE], &[0, 0], "the original bytes");

        let mut torn = bytes;
        torn[2 * STRIDE - 1] ^= 0xff;
        assert!(!apply_fixups(&mut torn), "a stride written half-way");
    }

    #[test]
    fn runs_decode_with_signed_offsets_and_holes() {
        // 4 clusters at 0x20, 2 at 0x20 - 0x10, a 3-cluster hole, 1 at +0x100.
        let bytes = [
            0x11, 0x04, 0x20, 0x11, 0x02, 0xf0, 0x01, 0x03, 0x21, 0x01, 0x00,
            0x01, 0x00,
        ];
        assert_eq!(
            decode_runs(&bytes),
            Some(vec![
                Run {
                    start: Some(0x20),
                    length: 4
                },
                Run {
                    start: Some(0x10),
                    length: 2
                },
                Run {
                    start: None,
                    length: 3
                },
                Run {
                    start: Some(0x110),
                    length: 1
                },
            ])
        );
        assert_eq!(decode_runs(&[0x11, 0x04]), None, "cut short");
    }

    #[test]
    fn a_record_yields_names_sizes_and_time() {
        let record = parse(file(41, "notes.txt", 4096, 1000));
        assert_eq!(record.names, vec![(41, "notes.txt".into())]);
        assert_eq!((record.allocated, record.apparent), (4096, 1000));
        assert_eq!(record.modified, Some(2_000));
        assert!(!record.is_dir);
        assert_eq!(record.base, None);
    }

    #[test]
    fn sparse_and_compressed_streams_count_what_they_occupy() {
        let record = parse(
            Builder::new(FILE, 0)
                .file_name(41, "disk.vhdx", 1)
                .non_resident(
                    "",
                    SPARSE,
                    &[0x11, 0x01, 0x10],
                    (1 << 30, 1 << 30, 4096),
                )
                .non_resident(
                    "Zone.Identifier",
                    0,
                    &[0x11, 0x01, 0x40],
                    (4096, 26, 0),
                )
                .finish(),
        );
        assert_eq!(record.allocated, 4096 + 4096, "sparse, plus a stream");
        assert_eq!(record.apparent, 1 << 30, "only the unnamed stream");
    }

    #[test]
    fn free_records_and_other_bytes_are_not_records() {
        let mut free = Builder::new(0, 0).file_name(5, "gone", 1).finish();
        assert_eq!(parse_record(&mut free, 4096), None);
        let mut zeros = vec![0_u8; 1024];
        assert_eq!(parse_record(&mut zeros, 4096), None);
    }

    #[test]
    fn the_table_builds_the_tree_the_walk_would() {
        let index = table().index();
        let tobi = index.resolve(["Users", "tobi"]).expect("tobi");
        assert_eq!(tobi, 41);
        let mut tree = index.build(tobi, "tobi", &options());
        crate::tree::aggregate(&mut tree, crate::tree::Metric::Bytes);

        assert_eq!(named(&tree, "notes.txt").bytes, 4096, "allocated");
        assert_eq!(named(named(&tree, ".cache"), "blob.bin").bytes, 65_536);
        let junction = named(&tree, "Application Data");
        assert_eq!(junction.kind, NodeKind::Symlink, "not entered");
        assert!(tree.child_named("ONE~1.BIN").is_none(), "8.3 alias");
        let one = named(&tree, "one.bin");
        let same = named(named(&tree, ".cache"), "same.bin");
        assert_eq!(one.inode, same.inode, "one file, two names");
        assert!(one.inode.is_some());
        assert_eq!(one.modified, 4_000);
    }

    #[test]
    fn paths_resolve_without_regard_to_case() {
        let index = table().index();
        assert_eq!(index.resolve(["users", "TOBI"]), Some(41));
        assert_eq!(index.resolve(["Users", "nobody"]), None);
        assert_eq!(index.resolve(std::iter::empty()), Some(ROOT_RECORD));
    }

    #[test]
    fn scan_options_apply_as_they_do_to_the_walk() {
        let index = table().index();
        let tobi = index.resolve(["Users", "tobi"]).expect("tobi");
        let no_hidden = ScanOptions {
            include_hidden: false,
            ..options()
        };
        let tree = index.build(tobi, "tobi", &no_hidden);
        assert!(tree.child_named(".cache").is_none());

        let apparent = ScanOptions {
            apparent_size: true,
            ..options()
        };
        let tree = index.build(tobi, "tobi", &apparent);
        assert_eq!(named(&tree, "notes.txt").own_bytes, 1000);

        let shallow = ScanOptions {
            max_depth: Some(0),
            ..options()
        };
        let tree = index.build(tobi, "tobi", &shallow);
        assert!(tree.children.is_empty(), "depth 0 lists nothing below");
    }

    #[test]
    fn the_volume_root_holds_the_system_files() {
        let index = table().index();
        let tree = index.build(ROOT_RECORD, r"C:\", &options());
        assert!(tree.child_named("$MFT").is_some());
        assert!(tree.child_named(".").is_none(), "the root is not its child");
    }

    #[test]
    fn extension_records_fold_into_their_base() {
        let mut table = Table::default();
        table.add(5, parse(directory(5, ".")));
        table.add(
            60,
            parse(
                Builder::new(FILE, 0)
                    .standard(5_000)
                    .file_name(5, "big.iso", 1)
                    .finish(),
            ),
        );
        // The stream's sizes live in record 61, which points back at 60.
        table.add(
            61,
            parse(
                Builder::new(FILE, 60)
                    .non_resident(
                        "",
                        0,
                        &[0x11, 0x08, 0x10],
                        (32_768, 30_000, 0),
                    )
                    .finish(),
            ),
        );
        let index = table.index();
        let tree = index.build(ROOT_RECORD, r"C:\", &options());
        assert_eq!(named(&tree, "big.iso").own_bytes, 32_768);
        assert_eq!(tree.children.len(), 1, "the extension is not a file");
    }

    #[test]
    fn pieces_cover_the_table_in_order_across_runs_and_holes() {
        let geometry = Geometry {
            cluster: 4096,
            record: 1024,
            mft_offset: 0,
        };
        let runs = [
            Run {
                start: Some(100),
                length: 3,
            },
            Run {
                start: None,
                length: 1,
            },
            Run {
                start: Some(500),
                length: 4,
            },
        ];
        // Eight clusters, the last one only half used by the table.
        let size = 7 * 4096 + 2048;
        let pieces = cut(&runs, geometry, size, 8192);
        let firsts: Vec<u64> =
            pieces.iter().map(|piece| piece.first_record).collect();
        assert_eq!(firsts, vec![0, 8, 16, 24]);
        assert_eq!(
            pieces[1].segments,
            vec![
                Segment {
                    offset: Some(102 * 4096),
                    length: 4096
                },
                Segment {
                    offset: None,
                    length: 4096
                },
            ],
            "a piece that crosses into a hole"
        );
        assert_eq!(
            pieces[3].segments,
            vec![Segment {
                offset: Some(502 * 4096),
                length: 8192
            }],
            "whole clusters, even past the table's end"
        );
        assert_eq!(pieces[3].records, 6, "but only the table's records");
        let records: usize = pieces.iter().map(|piece| piece.records).sum();
        assert_eq!(records as u64, size / 1024);
        assert!(pieces.iter().all(|piece| piece.length % 4096 == 0));
    }

    #[test]
    fn free_stretches_are_not_read_and_the_rest_is_merged() {
        let geometry = Geometry {
            cluster: 4096,
            record: 1024,
            mft_offset: 0,
        };
        // 1 MiB of table in one run: 16 granules of 64 records each.
        let runs = [Run {
            start: Some(1000),
            length: 256,
        }];
        let size = 1 << 20;
        let mut bits = vec![0_u8; 1024 / 8];
        // In use: record 3 (granule 0), 70 (granule 1), 700 (granule 10).
        for record in [3_usize, 70, 700] {
            bits[record / 8] |= 1 << (record % 8);
        }
        let pieces = plan_pieces(&runs, geometry, size, 8 << 20, Some(&bits));
        assert_eq!(pieces.len(), 2, "granules 0-1 merge; 10 stands alone");
        assert_eq!(pieces[0].first_record, 0);
        assert_eq!(pieces[0].records, 128);
        assert_eq!(
            pieces[0].segments,
            vec![Segment {
                offset: Some(1000 * 4096),
                length: 128 << 10
            }]
        );
        assert_eq!(pieces[1].first_record, 640);
        assert_eq!(
            pieces[1].segments,
            vec![Segment {
                offset: Some(1000 * 4096 + (640 << 10)),
                length: 64 << 10
            }]
        );

        let everything = plan_pieces(&runs, geometry, size, 8 << 20, None);
        assert_eq!(everything.len(), 1, "no bitmap: read it all, in one");
        assert_eq!(everything[0].records, 1024);
    }

    #[test]
    fn a_long_link_target_leaves_its_tag_to_be_read() {
        let mut header = vec![0_u8; 0x48];
        header[0..4].copy_from_slice(&ATTR_REPARSE_POINT.to_le_bytes());
        header[4..8].copy_from_slice(&0x48_u32.to_le_bytes());
        header[8] = 1;
        header[0x20..0x22].copy_from_slice(&0x40_u16.to_le_bytes());
        header[0x40..0x43].copy_from_slice(&[0x11, 0x01, 0x77]);
        let record = parse(
            Builder::new(DIRECTORY, 0)
                .file_name(41, "node_modules", 1)
                .attribute(&header)
                .finish(),
        );
        assert_eq!(record.reparse_tag, None);
        assert_eq!(record.reparse_cluster, Some(0x77));

        let mut table = Table::default();
        table.add(5, parse(directory(5, ".")));
        table.add(41, parse(directory(5, "pkg")));
        table.add(70, record);
        assert_eq!(table.unresolved_reparse(), vec![(70, 0x77)]);
        table.set_reparse_tag(70, 0xa000_0003);
        assert!(table.unresolved_reparse().is_empty());
        let index = table.index();
        let tree = index.build(41, "pkg", &options());
        assert_eq!(named(&tree, "node_modules").kind, NodeKind::Symlink);
    }

    #[test]
    fn the_table_extent_comes_from_record_zero() {
        let mut zero = Builder::new(FILE, 0)
            .file_name(5, "$MFT", 1)
            .non_resident(
                "",
                0,
                &[0x21, 0x10, 0x00, 0x01, 0x11, 0x08, 0x40],
                (24 * 4096, 24 * 4096, 0),
            )
            .finish();
        let extent = table_extent(&mut zero).expect("extent");
        assert_eq!(extent.size, 24 * 4096);
        assert_eq!(
            extent.runs,
            vec![
                Run {
                    start: Some(0x100),
                    length: 16
                },
                Run {
                    start: Some(0x140),
                    length: 8
                },
            ]
        );
        assert_eq!(extent.bitmap, None, "no bitmap: every record is read");
    }

    #[test]
    fn the_in_use_bitmap_is_found_beside_the_table() {
        let mut zero = Builder::new(FILE, 0)
            .file_name(5, "$MFT", 1)
            .non_resident("", 0, &[0x11, 0x08, 0x40], (8 * 4096, 8 * 4096, 0))
            .resident(ATTR_BITMAP, "", &[0b1010_0001, 0xff])
            .finish();
        let extent = table_extent(&mut zero).expect("extent");
        assert_eq!(
            extent.bitmap,
            Some(Stream::Resident(vec![0b1010_0001, 0xff]))
        );
    }
}
