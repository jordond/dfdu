//! File types by extension, and the sizes of the files of chosen types below a node.
//!
//! A file's type comes from its extension alone (the text after the last `.`, ASCII case
//! ignored); contents are never read. [`TypeFilter`] picks files by type or by extension, and
//! [`sum`] adds up the matching files below a node, with a per-type and per-extension
//! [`Breakdown`] if asked for.

use std::collections::HashMap;
use std::fmt;

use crate::{Ix, Kind, Totals, Tree};

/// A broad kind of file.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum FileType {
    Video,
    Audio,
    Image,
    Document,
    Archive,
    DiskImage,
    Code,
    Executable,
    Font,
    Data,
    /// No extension, or one not in the table.
    Other,
}

impl FileType {
    pub const ALL: [FileType; 11] = [
        FileType::Video,
        FileType::Audio,
        FileType::Image,
        FileType::Document,
        FileType::Archive,
        FileType::DiskImage,
        FileType::Code,
        FileType::Executable,
        FileType::Font,
        FileType::Data,
        FileType::Other,
    ];

    pub fn name(self) -> &'static str {
        match self {
            FileType::Video => "video",
            FileType::Audio => "audio",
            FileType::Image => "image",
            FileType::Document => "document",
            FileType::Archive => "archive",
            FileType::DiskImage => "disk-image",
            FileType::Code => "code",
            FileType::Executable => "executable",
            FileType::Font => "font",
            FileType::Data => "data",
            FileType::Other => "other",
        }
    }

    /// The type named `s`; also accepts a few plural and common names (`videos`, `pictures`,
    /// `photo`, `docs`).
    pub fn parse(s: &str) -> Option<FileType> {
        let s = s.trim().to_ascii_lowercase();
        let s = s.strip_suffix('s').filter(|t| !t.is_empty()).unwrap_or(&s);
        Some(match s {
            "video" | "movie" => FileType::Video,
            "audio" | "music" | "sound" => FileType::Audio,
            "image" | "picture" | "photo" | "pic" => FileType::Image,
            "document" | "doc" => FileType::Document,
            "archive" => FileType::Archive,
            "disk-image" | "diskimage" | "disk" => FileType::DiskImage,
            "code" | "source" => FileType::Code,
            "executable" | "binarie" | "binary" | "program" | "app" => FileType::Executable,
            "font" => FileType::Font,
            "data" | "database" => FileType::Data,
            "other" => FileType::Other,
            _ => return None,
        })
    }

    fn bit(self) -> u16 {
        1 << self as u16
    }
}

impl fmt::Display for FileType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A lower-case extension of at most [`Ext::MAX`] bytes, without the dot.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct Ext {
    len: u8,
    bytes: [u8; Ext::MAX],
}

impl Ext {
    pub const MAX: usize = 10;

    /// The extension of a file named `name`: the text after the last `.`, if the name has
    /// something before the dot and the extension is 1 to [`Ext::MAX`] ASCII letters and
    /// digits. `archive.tar.gz` gives `gz`; `.bashrc` and `notes.` give none.
    pub fn of(name: &[u8]) -> Option<Ext> {
        let dot = name.iter().rposition(|&b| b == b'.')?;
        Ext::new(&name[dot + 1..]).filter(|_| dot > 0)
    }

    /// `s` as an extension (a leading dot is dropped); `None` when it cannot be one.
    pub fn new(s: &[u8]) -> Option<Ext> {
        let s = s.strip_prefix(b".").unwrap_or(s);
        if s.is_empty() || s.len() > Ext::MAX || !s.iter().all(u8::is_ascii_alphanumeric) {
            return None;
        }
        let mut bytes = [0; Ext::MAX];
        for (d, b) in bytes.iter_mut().zip(s) {
            *d = b.to_ascii_lowercase();
        }
        Some(Ext {
            len: s.len() as u8,
            bytes,
        })
    }

    pub fn as_str(&self) -> &str {
        // Only ASCII letters and digits are stored.
        std::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }

    pub fn file_type(&self) -> FileType {
        type_of_ext(self.as_str())
    }
}

impl fmt::Debug for Ext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, ".{}", self.as_str())
    }
}

impl fmt::Display for Ext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The type of a file named `name`.
pub fn type_of(name: &[u8]) -> FileType {
    Ext::of(name).map_or(FileType::Other, |e| e.file_type())
}

fn type_of_ext(e: &str) -> FileType {
    use FileType::*;
    match e {
        "mp4" | "m4v" | "mov" | "mkv" | "avi" | "webm" | "wmv" | "flv" | "mpg" | "mpeg" | "m2v"
        | "mts" | "m2ts" | "3gp" | "ogv" | "vob" | "mxf" | "rmvb" | "braw" | "r3d" => Video,
        "mp3" | "m4a" | "aac" | "flac" | "wav" | "aif" | "aiff" | "ogg" | "oga" | "opus"
        | "wma" | "alac" | "mid" | "midi" | "caf" | "ape" | "dsf" | "m4b" | "amr" => Audio,
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "avif" | "bmp" | "tif"
        | "tiff" | "svg" | "ico" | "icns" | "psd" | "raw" | "cr2" | "cr3" | "nef" | "arw"
        | "dng" | "orf" | "rw2" | "raf" | "xcf" | "exr" | "hdr" | "jxl" | "kra" | "afphoto" => {
            Image
        }
        "pdf" | "doc" | "docx" | "odt" | "rtf" | "txt" | "md" | "markdown" | "rst" | "tex"
        | "epub" | "mobi" | "azw3" | "pages" | "key" | "numbers" | "xls" | "xlsx" | "ods"
        | "ppt" | "pptx" | "odp" | "djvu" | "org" | "adoc" => Document,
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "tbz2" | "xz" | "txz" | "zst" | "lz4" | "lzma"
        | "7z" | "rar" | "cab" | "jar" | "war" | "whl" | "crate" | "gem" | "nupkg" | "cpio"
        | "lz" | "z" | "sit" | "sitx" => Archive,
        "iso" | "dmg" | "img" | "vmdk" | "vdi" | "vhd" | "vhdx" | "qcow2" | "qcow" | "hdd"
        | "sparseimage" | "sparsebundle" | "toast" | "wim" | "esd" | "xip" => DiskImage,
        "rs" | "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "m" | "mm" | "swift" | "go"
        | "py" | "pyi" | "rb" | "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" | "java" | "kt"
        | "kts" | "scala" | "cs" | "fs" | "php" | "pl" | "lua" | "sh" | "bash" | "zsh" | "fish"
        | "ps1" | "zig" | "nim" | "dart" | "ex" | "exs" | "erl" | "hs" | "ml" | "mli" | "clj"
        | "el" | "vim" | "html" | "htm" | "css" | "scss" | "sass" | "less" | "vue" | "svelte"
        | "sql" | "toml" | "yaml" | "yml" | "ini" | "cfg" | "gradle" | "cmake" | "mk" | "proto"
        | "graphql" => Code,
        "exe" | "dll" | "so" | "dylib" | "a" | "lib" | "o" | "obj" | "rlib" | "rmeta" | "wasm"
        | "bin" | "elf" | "app" | "msi" | "msix" | "pkg" | "mpkg" | "deb" | "rpm" | "apk"
        | "aab" | "ipa" | "appimage" | "snap" | "flatpak" | "class" | "pyc" | "pdb" | "dsym"
        | "pch" | "node" | "sys" | "efi" => Executable,
        "ttf" | "otf" | "woff" | "woff2" | "ttc" | "fon" | "pfb" | "dfont" => Font,
        "json" | "jsonl" | "ndjson" | "csv" | "tsv" | "xml" | "parquet" | "arrow" | "feather"
        | "avro" | "orc" | "db" | "sqlite" | "sqlite3" | "mdb" | "accdb" | "dbf" | "h5"
        | "hdf5" | "npy" | "npz" | "pkl" | "pickle" | "mat" | "nc" | "log" | "dat" | "bak"
        | "dump" | "pb" | "onnx" | "safetensors" | "gguf" | "ckpt" | "pt" | "pth" | "tfrecord" => {
            Data
        }
        _ => Other,
    }
}

/// Which files to count: those of any of the chosen types or with any of the chosen
/// extensions. An empty filter matches every file.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct TypeFilter {
    types: u16,
    exts: Vec<Ext>,
}

impl TypeFilter {
    /// Reads a comma- or space-separated list of type names and extensions: `video, .iso,
    /// pdf`. A word with a leading dot, or that is not a type name, is an extension.
    pub fn parse(s: &str) -> Result<TypeFilter, String> {
        let mut f = TypeFilter::default();
        for w in s.split([',', ' ']).filter(|w| !w.is_empty()) {
            match FileType::parse(w).filter(|_| !w.starts_with('.')) {
                Some(t) => f.toggle_type(t),
                None => match Ext::new(w.as_bytes()) {
                    Some(e) => f.add_ext(e),
                    None => return Err(format!("`{w}` is neither a file type nor an extension")),
                },
            }
        }
        Ok(f)
    }

    pub fn is_empty(&self) -> bool {
        self.types == 0 && self.exts.is_empty()
    }

    pub fn has_type(&self, t: FileType) -> bool {
        self.types & t.bit() != 0
    }

    pub fn has_ext(&self, e: &Ext) -> bool {
        self.exts.contains(e)
    }

    pub fn toggle_type(&mut self, t: FileType) {
        self.types ^= t.bit();
    }

    pub fn add_ext(&mut self, e: Ext) {
        if !self.exts.contains(&e) {
            self.exts.push(e);
        }
    }

    pub fn toggle_ext(&mut self, e: Ext) {
        match self.exts.iter().position(|x| *x == e) {
            Some(i) => {
                self.exts.remove(i);
            }
            None => self.exts.push(e),
        }
    }

    pub fn types(&self) -> impl Iterator<Item = FileType> + '_ {
        FileType::ALL.into_iter().filter(|&t| self.has_type(t))
    }

    pub fn exts(&self) -> &[Ext] {
        &self.exts
    }

    /// Whether a file named `name` counts.
    pub fn matches(&self, name: &[u8]) -> bool {
        if self.is_empty() {
            return true;
        }
        let ext = Ext::of(name);
        let t = ext.map_or(FileType::Other, |e| e.file_type());
        self.has_type(t) || ext.is_some_and(|e| self.exts.contains(&e))
    }
}

impl fmt::Display for TypeFilter {
    /// `video, audio, .iso`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let words = self
            .types()
            .map(|t| t.name().to_owned())
            .chain(self.exts.iter().map(|e| format!(".{e}")));
        f.write_str(&words.collect::<Vec<_>>().join(", "))
    }
}

/// Files below a node by type and by extension. `items` in each [`Totals`] counts files.
#[derive(Clone, Debug, Default)]
pub struct Breakdown {
    pub by_type: [Totals; FileType::ALL.len()],
    pub by_ext: HashMap<Ext, Totals>,
}

impl Breakdown {
    fn add(&mut self, name: &[u8], t: &Totals) {
        let ext = Ext::of(name);
        let ty = ext.map_or(FileType::Other, |e| e.file_type());
        self.by_type[ty as usize].add(t);
        if let Some(e) = ext {
            self.by_ext.entry(e).or_default().add(t);
        }
    }

    pub fn merge(&mut self, o: &Breakdown) {
        for (a, b) in self.by_type.iter_mut().zip(&o.by_type) {
            a.add(b);
        }
        for (e, t) in &o.by_ext {
            self.by_ext.entry(*e).or_default().add(t);
        }
    }

    /// The extensions, largest first by `size`, then by name.
    pub fn exts_by(&self, size: impl Fn(&Totals) -> u64) -> Vec<(Ext, Totals)> {
        let mut v: Vec<(Ext, Totals)> = self.by_ext.iter().map(|(e, t)| (*e, *t)).collect();
        v.sort_by(|a, b| size(&b.1).cmp(&size(&a.1)).then(a.0.cmp(&b.0)));
        v
    }
}

/// The totals of the files at or below `ix` that `filter` matches, with `items` counting those
/// files only. Each matching file is also added to `breakdown`, when given. Directories,
/// symlinks and other entries count nothing themselves; data shared between files that is
/// counted at a directory (btrfs extents, hard links charged once) is left out. Linear in the
/// size of the subtree.
pub fn sum(
    tree: &Tree,
    ix: Ix,
    filter: &TypeFilter,
    mut breakdown: Option<&mut Breakdown>,
) -> Totals {
    let mut out = Totals::default();
    let mut add = |c: Ix| {
        if tree.kind(c) == Kind::File {
            let name = tree.name(c);
            if filter.matches(name) {
                let t = tree.total(c);
                let t = Totals {
                    items: 1,
                    errors: 0,
                    ..t
                };
                out.add(&t);
                if let Some(b) = breakdown.as_deref_mut() {
                    b.add(name, &t);
                }
            }
        }
    };
    if tree.dir(ix).is_none() {
        add(ix);
        return out;
    }
    let mut stack = vec![ix];
    while let Some(d) = stack.pop() {
        for c in tree.children(d) {
            if tree.dir(c).is_some() {
                stack.push(c);
            } else {
                add(c);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Meta;

    #[test]
    fn extensions() {
        assert_eq!(Ext::of(b"Trip.MOV").unwrap().as_str(), "mov");
        assert_eq!(Ext::of(b"src.tar.gz").unwrap().as_str(), "gz");
        assert!(Ext::of(b".bashrc").is_none());
        assert!(Ext::of(b"notes.").is_none());
        assert!(Ext::of(b"Makefile").is_none());
        assert!(Ext::of(b"x.backup-2023").is_none());
        assert!(Ext::of(b"x.abcdefghijk").is_none());
        assert_eq!(type_of(b"a.HEIC"), FileType::Image);
        assert_eq!(type_of(b"a.flac"), FileType::Audio);
        assert_eq!(type_of(b"Ubuntu.iso"), FileType::DiskImage);
        assert_eq!(type_of(b"README"), FileType::Other);
        assert_eq!(type_of(b"a.xyz"), FileType::Other);
    }

    #[test]
    fn every_type_has_a_name_that_parses_back() {
        for t in FileType::ALL {
            assert_eq!(FileType::parse(t.name()), Some(t));
        }
        assert_eq!(FileType::parse("Videos"), Some(FileType::Video));
        assert_eq!(FileType::parse("pictures"), Some(FileType::Image));
        assert_eq!(FileType::parse("s"), None);
    }

    #[test]
    fn filters() {
        let f = TypeFilter::parse("video, .iso pdf").unwrap();
        assert!(f.matches(b"a.mkv"));
        assert!(f.matches(b"b.ISO"));
        assert!(f.matches(b"c.pdf"));
        assert!(!f.matches(b"d.epub"));
        assert!(!f.matches(b"README"));
        assert_eq!(f.to_string(), "video, .iso, .pdf");
        // A type name with a dot is an extension.
        let f = TypeFilter::parse(".data").unwrap();
        assert!(f.matches(b"x.data") && !f.matches(b"x.csv"));
        assert!(TypeFilter::parse("a/b").is_err());
        assert!(TypeFilter::default().matches(b"anything"));
        let other = TypeFilter::parse("other").unwrap();
        assert!(other.matches(b"README") && other.matches(b"a.xyz") && !other.matches(b"a.mp3"));
    }

    #[test]
    fn sums_count_matching_files_below() {
        let meta = |kind, alloc| Meta {
            kind,
            apparent: alloc,
            alloc,
            mtime: 0,
            dev: 1,
            ino: alloc,
            nlink: 1,
            mount: 0,
        };
        let mut t = Tree::new();
        let root = t.new_root(b"/r", &meta(Kind::Dir, 0), true);
        let sub = t.add_dir(root, b"sub", &meta(Kind::Dir, 0));
        t.add_file(root, b"a.mp4", &meta(Kind::File, 100), false, None);
        t.add_file(root, b"b.txt", &meta(Kind::File, 10), false, None);
        t.add_file(sub, b"c.MKV", &meta(Kind::File, 1000), false, None);
        t.add_file(sub, b"d.mp3", &meta(Kind::File, 50), false, None);
        let video = TypeFilter::parse("video").unwrap();
        let mut b = Breakdown::default();
        let s = sum(&t, root, &video, Some(&mut b));
        assert_eq!((s.alloc, s.items), (1100, 2));
        assert_eq!(sum(&t, sub, &video, None).alloc, 1000);
        let all = sum(&t, root, &TypeFilter::default(), None);
        assert_eq!((all.alloc, all.items), (1160, 4));
        assert_eq!(b.by_type[FileType::Video as usize].alloc, 1100);
        assert_eq!(b.by_type[FileType::Audio as usize].alloc, 0);
        let exts = b.exts_by(|t| t.alloc);
        assert_eq!(exts[0].0.as_str(), "mkv");
        assert_eq!(exts[1].0.as_str(), "mp4");
    }

    /// Cost of the walk the browser does per snapshot while a type filter is on (and once more
    /// for the picker's breakdown), on 1M entries in 10k directories.
    /// `cargo test --release -p dfdu-tree sum_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn sum_cost() {
        let meta = |kind| Meta {
            kind,
            apparent: 1000,
            alloc: 4096,
            mtime: 0,
            dev: 1,
            ino: 0,
            nlink: 1,
            mount: 0,
        };
        const NAMES: [&str; 8] = [
            "a.mp4", "b.txt", "c.rs", "d.jpg", "Makefile", "e.tar.gz", "f.json", "g.o",
        ];
        let mut t = Tree::new();
        let root = t.new_root(b"/r", &meta(Kind::Dir), true);
        for d in 0..100 {
            let top = t.add_dir(root, format!("d{d}").as_bytes(), &meta(Kind::Dir));
            for s in 0..100 {
                let sub = t.add_dir(top, format!("s{s}").as_bytes(), &meta(Kind::Dir));
                for f in 0..100 {
                    let n = format!("{f}{}", NAMES[f % NAMES.len()]);
                    t.add_file(sub, n.as_bytes(), &meta(Kind::File), false, None);
                }
            }
        }
        let video = TypeFilter::parse("video,.json").unwrap();
        for (what, f, b) in [
            ("filter", &video, false),
            ("breakdown", &TypeFilter::default(), true),
        ] {
            let mut best = std::time::Duration::MAX;
            for _ in 0..5 {
                let mut bd = Breakdown::default();
                let start = std::time::Instant::now();
                let s = sum(&t, root, f, b.then_some(&mut bd));
                best = best.min(start.elapsed());
                assert!(s.items > 0);
            }
            eprintln!("{what}: {best:?} for 1M entries");
        }
    }
}
