//! Reading a Windows executable's headers and version resource: the game's properties for
//! target filters, and the versions of DLLs the loader did not build (`.asi` files, UE4SS).
//!
//! Parsed from the file's bytes rather than through `GetFileVersionInfoW`: the loader may
//! itself be standing in for `version.dll`, and a parser works the same on every host, which
//! keeps it testable here. Only the parts that are read are loaded, so a 90 MB game binary
//! costs its headers and its resource section.
//!
//! Layouts: Microsoft's "PE Format" page (COFF header, optional header, data directories,
//! debug directory) and the `VS_VERSIONINFO`, `StringFileInfo` and `VarFileInfo` pages.

use std::io::{Read, Seek, SeekFrom};

/// Where the parser reads from: a file, or bytes already in memory.
pub trait Source {
    /// `len` bytes at `offset`, or `None` when the source is shorter.
    fn read_at(&mut self, offset: u64, len: usize) -> Option<Vec<u8>>;
}

impl Source for &[u8] {
    fn read_at(&mut self, offset: u64, len: usize) -> Option<Vec<u8>> {
        let start = usize::try_from(offset).ok()?;
        self.get(start..start.checked_add(len)?).map(<[u8]>::to_vec)
    }
}

/// A seekable reader, such as a [`std::fs::File`], as a [`Source`].
pub struct Reader<R>(pub R);

impl<R: Read + Seek> Source for Reader<R> {
    fn read_at(&mut self, offset: u64, len: usize) -> Option<Vec<u8>> {
        self.0.seek(SeekFrom::Start(offset)).ok()?;
        let mut out = vec![0; len];
        self.0.read_exact(&mut out).ok()?;
        Some(out)
    }
}

/// The most of one directory the parser reads: resource sections hold icons too, but nothing
/// sane needs more than this.
const MAX_DIRECTORY: usize = 64 << 20;

/// Bytes as lower-case hex.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, b| {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// One section: where its bytes are in memory and in the file.
#[derive(Debug, Clone, Copy)]
struct Section {
    address: u32,
    size: u32,
    raw_offset: u32,
    raw_size: u32,
}

/// The headers of a PE image, and what the parser found through its data directories.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Image {
    /// COFF `Machine`: 0x8664 x64, 0x14C x86, 0xAA64 ARM64.
    pub machine: u16,
    /// 32 or 64, from the optional header's magic.
    pub bitness: u32,
    /// COFF `TimeDateStamp`: link time, or a hash for reproducible builds.
    pub timestamp: u32,
    /// COFF `Characteristics`.
    pub characteristics: u16,
    /// `MajorLinkerVersion`, `MinorLinkerVersion`.
    pub linker_version: [u16; 2],
    /// `MajorOperatingSystemVersion`, minor.
    pub os_version: [u16; 2],
    /// `MajorImageVersion`, minor.
    pub image_version: [u16; 2],
    /// `MajorSubsystemVersion`, minor.
    pub subsystem_version: [u16; 2],
    /// `Subsystem`: 2 Windows GUI, 3 console.
    pub subsystem: u16,
    /// `DllCharacteristics`.
    pub dll_characteristics: u16,
    /// `CheckSum`, often 0.
    pub checksum: u32,
    /// `SizeOfImage`.
    pub size_of_image: u32,
    /// `AddressOfEntryPoint`.
    pub entry_point: u32,
    /// Whether the security directory holds an Authenticode signature (not verified).
    pub signed: bool,
    /// The `CodeView` record of the debug directory: the PDB the image was linked with.
    pub pdb: Option<Pdb>,
    /// The .NET runtime version string, for a managed image.
    pub clr: Option<Clr>,
    /// The version resource.
    pub version: Option<VersionInfo>,
}

/// The PDB an image was linked with; GUID and age together name exactly one build.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pdb {
    /// GUID, lower-case `8-4-4-4-12` form.
    pub guid: String,
    /// Age.
    pub age: u32,
    /// Path as the linker wrote it.
    pub path: String,
    /// The symbol server key: the GUID's 32 hex digits upper-case, then the age in hex.
    pub build_id: String,
}

/// The CLI header of a managed (.NET) image.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Clr {
    /// `Flags`: 0x1 IL only, 0x2 32-bit required, 0x8 strong-name signed.
    pub flags: u32,
    /// The metadata root's version string, for example `v4.0.30319`.
    pub runtime_version: String,
}

/// A version resource: the fixed part and every string table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VersionInfo {
    /// The resource's language ID in the resource directory.
    pub language: u16,
    /// `VS_FIXEDFILEINFO`, when present.
    pub fixed: Option<Fixed>,
    /// `VarFileInfo\Translation`: (language, codepage) pairs.
    pub translations: Vec<(u16, u16)>,
    /// String tables by key (8 hex digits, language then codepage, lower-case), each a list
    /// of (name, value) in file order.
    pub tables: Vec<(String, Vec<(String, String)>)>,
}

/// `VS_FIXEDFILEINFO`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fixed {
    /// `dwFileVersionMS`/`LS` as four parts.
    pub file_version: [u16; 4],
    /// `dwProductVersionMS`/`LS` as four parts.
    pub product_version: [u16; 4],
    /// `dwFileFlags & dwFileFlagsMask`: 0x1 debug, 0x2 prerelease, 0x4 patched, 0x8 private
    /// build, 0x20 special build.
    pub flags: u32,
    /// `dwFileOS`: 0x40004 `VOS_NT_WINDOWS32`.
    pub os: u32,
    /// `dwFileType`: 1 app, 2 DLL (Unreal games say DLL too).
    pub file_type: u32,
    /// `dwFileSubtype`.
    pub subtype: u32,
}

impl VersionInfo {
    /// The string table a reader of the resource would use: the first translation that has
    /// a table, then US English in Unicode, Windows-1252 and neutral, then the first table.
    #[must_use]
    pub fn table(&self) -> Option<&[(String, String)]> {
        let keys = self
            .translations
            .iter()
            .map(|(lang, cp)| format!("{lang:04x}{cp:04x}"))
            .chain(["040904b0", "040904e4", "04090000"].map(str::to_owned));
        for key in keys {
            if let Some((_, table)) = self.tables.iter().find(|(k, _)| *k == key) {
                return Some(table);
            }
        }
        self.tables.first().map(|(_, table)| table.as_slice())
    }

    /// One string of [`VersionInfo::table`], such as `ProductName`, matched ignoring case.
    #[must_use]
    pub fn string(&self, name: &str) -> Option<&str> {
        self.table()?
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Parse an image's headers and the directories behind them. `None` when it is not a PE
/// image; a broken directory only leaves its part empty.
pub fn parse(source: &mut impl Source) -> Option<Image> {
    let dos = source.read_at(0, 64)?;
    if dos.get(..2)? != b"MZ" {
        return None;
    }
    let pe = u64::from(u32_at(&dos, 0x3C)?);
    let head = source.read_at(pe, 24)?;
    if head.get(..4)? != b"PE\0\0" {
        return None;
    }
    let coff = &head[4..];
    let sections = u16_at(coff, 2)?;
    let optional_size = u16_at(coff, 16)?;
    let optional = source.read_at(pe + 24, usize::from(optional_size))?;
    let (bitness, directories_at) = match u16_at(&optional, 0)? {
        0x10B => (32, 96),
        0x20B => (64, 112),
        _ => return None,
    };
    let pair = |at: usize| Some([u16_at(&optional, at)?, u16_at(&optional, at + 2)?]);
    let mut image = Image {
        machine: u16_at(coff, 0)?,
        bitness,
        timestamp: u32_at(coff, 4)?,
        characteristics: u16_at(coff, 18)?,
        linker_version: [u16::from(*optional.get(2)?), u16::from(*optional.get(3)?)],
        entry_point: u32_at(&optional, 16)?,
        os_version: pair(40)?,
        image_version: pair(44)?,
        subsystem_version: pair(48)?,
        size_of_image: u32_at(&optional, 56)?,
        checksum: u32_at(&optional, 64)?,
        subsystem: u16_at(&optional, 68)?,
        dll_characteristics: u16_at(&optional, 70)?,
        ..Image::default()
    };
    let table_at = pe + 24 + u64::from(optional_size);
    let table = source.read_at(table_at, usize::from(sections) * 40)?;
    let sections: Vec<Section> = table
        .as_chunks::<40>()
        .0
        .iter()
        .filter_map(|s| {
            Some(Section {
                size: u32_at(s, 8)?,
                address: u32_at(s, 12)?,
                raw_size: u32_at(s, 16)?,
                raw_offset: u32_at(s, 20)?,
            })
        })
        .collect();
    let directory = |index: usize| -> Option<(u32, u32)> {
        let at = directories_at + index * 8;
        let pair = (u32_at(&optional, at)?, u32_at(&optional, at + 4)?);
        (pair.0 != 0 && pair.1 != 0).then_some(pair)
    };
    image.signed = directory(4).is_some();
    let mut read_rva = |rva: u32, len: u32| -> Option<Vec<u8>> {
        let len = usize::try_from(len).ok()?.min(MAX_DIRECTORY);
        let offset = file_offset(&sections, rva)?;
        source.read_at(offset, len)
    };
    if let Some((rva, size)) = directory(6) {
        image.pdb = read_rva(rva, size).and_then(|d| pdb(&d, &mut read_rva));
    }
    if let Some((rva, size)) = directory(14) {
        image.clr = read_rva(rva, size).and_then(|h| clr(&h, &mut read_rva));
    }
    if let Some((rva, size)) = directory(2) {
        image.version = read_rva(rva, size).and_then(|r| version_resource(&r, rva));
    }
    Some(image)
}

/// Where the byte at `rva` is in the file.
fn file_offset(sections: &[Section], rva: u32) -> Option<u64> {
    let s = sections.iter().find(|s| {
        let end = s.address.saturating_add(s.size.max(s.raw_size));
        rva >= s.address && rva < end
    })?;
    let delta = rva - s.address;
    (delta < s.raw_size).then(|| u64::from(s.raw_offset) + u64::from(delta))
}

/// The `CodeView` (`RSDS`) entry of the debug directory.
fn pdb(directory: &[u8], read_rva: &mut impl FnMut(u32, u32) -> Option<Vec<u8>>) -> Option<Pdb> {
    let entry = directory
        .as_chunks::<28>()
        .0
        .iter()
        .find(|e| u32_at(&e[..], 12) == Some(2))?;
    let data = read_rva(u32_at(entry, 20)?, u32_at(entry, 16)?)?;
    if data.get(..4)? != b"RSDS" {
        return None;
    }
    let g = data.get(4..20)?;
    let (d1, d2, d3) = (u32_at(g, 0)?, u16_at(g, 4)?, u16_at(g, 6)?);
    let tail = hex(&g[8..]);
    let guid = format!("{d1:08x}-{d2:04x}-{d3:04x}-{}-{}", &tail[..4], &tail[4..]);
    let age = u32_at(&data, 20)?;
    let path = data.get(24..)?;
    let path = &path[..path.iter().position(|&b| b == 0).unwrap_or(path.len())];
    Some(Pdb {
        build_id: format!("{d1:08X}{d2:04X}{d3:04X}{}{age:X}", tail.to_uppercase()),
        guid,
        age,
        path: String::from_utf8_lossy(path).into_owned(),
    })
}

/// The CLI header and the metadata root's version string.
fn clr(header: &[u8], read_rva: &mut impl FnMut(u32, u32) -> Option<Vec<u8>>) -> Option<Clr> {
    let flags = u32_at(header, 16)?;
    let root = read_rva(u32_at(header, 8)?, u32_at(header, 12)?);
    let runtime_version = root
        .filter(|r| r.get(..4) == Some(b"BSJB"))
        .and_then(|r| {
            let len = usize::try_from(u32_at(&r, 12)?).ok()?;
            let text = r.get(16..16 + len)?;
            let text = &text[..text.iter().position(|&b| b == 0).unwrap_or(text.len())];
            Some(String::from_utf8_lossy(text).into_owned())
        })
        .unwrap_or_default();
    Some(Clr {
        flags,
        runtime_version,
    })
}

/// `RT_VERSION`.
const RT_VERSION: u32 = 16;

/// The entries of one resource directory table: (name or ID, offset, is a subdirectory).
fn resource_entries(section: &[u8], at: usize) -> Vec<(u32, usize, bool)> {
    let (Some(named), Some(ids)) = (u16_at(section, at + 12), u16_at(section, at + 14)) else {
        return Vec::new();
    };
    (0..usize::from(named) + usize::from(ids))
        .filter_map(|i| {
            let entry = at + 16 + i * 8;
            let target = u32_at(section, entry + 4)?;
            let offset = usize::try_from(target & 0x7FFF_FFFF).ok()?;
            Some((u32_at(section, entry)?, offset, target & 0x8000_0000 != 0))
        })
        .collect()
}

/// Find the version resource in the resource section (which starts at `base` in memory).
fn version_resource(section: &[u8], base: u32) -> Option<VersionInfo> {
    let (_, types, _) = resource_entries(section, 0)
        .into_iter()
        .find(|(id, _, dir)| *id == RT_VERSION && *dir)?;
    let (_, names, _) = resource_entries(section, types)
        .into_iter()
        .find(|(_, _, dir)| *dir)?;
    let (language, data, _) = resource_entries(section, names)
        .into_iter()
        .find(|(_, _, dir)| !*dir)?;
    let rva = u32_at(section, data)?;
    let size = usize::try_from(u32_at(section, data + 4)?).ok()?;
    let start = usize::try_from(rva.checked_sub(base)?).ok()?;
    let mut info = version_info(section.get(start..start + size)?)?;
    info.language = u16::try_from(language & 0xFFFF).unwrap_or_default();
    Some(info)
}

/// One node of the `VS_VERSIONINFO` tree.
struct Node<'a> {
    key: String,
    /// Text nodes (`wType` 1): the value, as UTF-16 units up to the first NUL.
    text: Option<String>,
    value: &'a [u8],
    children: &'a [u8],
}

fn align4(at: usize) -> usize {
    (at + 3) & !3
}

/// The node at the start of `bytes`, and how many bytes it spans.
fn node(bytes: &[u8]) -> Option<(Node<'_>, usize)> {
    let length = usize::from(u16_at(bytes, 0)?).min(bytes.len());
    if length < 6 {
        // Padding, or the end of the block.
        return None;
    }
    let value_length = usize::from(u16_at(bytes, 2)?);
    let is_text = u16_at(bytes, 4)? == 1;
    let mut at = 6;
    let mut key = Vec::new();
    while let Some(unit) = u16_at(bytes, at) {
        at += 2;
        if unit == 0 {
            break;
        }
        key.push(unit);
    }
    at = align4(at).min(length);
    // Text lengths count UTF-16 units, though some writers count bytes; the NUL ends it either
    // way, and the node's own length bounds it.
    let value_bytes = if is_text {
        value_length * 2
    } else {
        value_length
    };
    let value = bytes.get(at..(at + value_bytes).min(length))?;
    let text = is_text.then(|| {
        let units: Vec<u16> = value
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .take_while(|&u| u != 0)
            .collect();
        String::from_utf16_lossy(&units)
    });
    let children = bytes.get(align4(at + value.len()).min(length)..length)?;
    let key = String::from_utf16_lossy(&key);
    Some((
        Node {
            key,
            text,
            value,
            children,
        },
        align4(length.max(6)),
    ))
}

/// Every child node in `bytes`.
fn children(mut bytes: &[u8]) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    while let Some((n, used)) = node(bytes) {
        out.push(n);
        bytes = bytes.get(used..).unwrap_or_default();
    }
    out
}

/// Parse a `VS_VERSIONINFO` resource.
#[must_use]
pub fn version_info(bytes: &[u8]) -> Option<VersionInfo> {
    let (root, _) = node(bytes)?;
    if root.key != "VS_VERSION_INFO" {
        return None;
    }
    let mut info = VersionInfo {
        fixed: fixed(root.value),
        ..VersionInfo::default()
    };
    for block in children(root.children) {
        match block.key.as_str() {
            "StringFileInfo" => {
                for table in children(block.children) {
                    let strings = children(table.children)
                        .into_iter()
                        .map(|s| (s.key, s.text.unwrap_or_default()))
                        .collect();
                    info.tables.push((table.key.to_ascii_lowercase(), strings));
                }
            }
            "VarFileInfo" => {
                for var in children(block.children) {
                    if var.key == "Translation" {
                        info.translations = var
                            .value
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|c| {
                                (
                                    u16::from_le_bytes([c[0], c[1]]),
                                    u16::from_le_bytes([c[2], c[3]]),
                                )
                            })
                            .collect();
                    }
                }
            }
            _ => {}
        }
    }
    Some(info)
}

fn fixed(value: &[u8]) -> Option<Fixed> {
    if u32_at(value, 0)? != 0xFEEF_04BD {
        return None;
    }
    let parts = |at: usize| -> Option<[u16; 4]> {
        let (ms, ls) = (u32_at(value, at)?, u32_at(value, at + 4)?);
        // The high and low halves, which is exactly what the truncation keeps.
        #[allow(clippy::cast_possible_truncation)]
        Some([(ms >> 16) as u16, ms as u16, (ls >> 16) as u16, ls as u16])
    };
    Some(Fixed {
        file_version: parts(8)?,
        product_version: parts(16)?,
        flags: u32_at(value, 28)? & u32_at(value, 24)?,
        os: u32_at(value, 32)?,
        file_type: u32_at(value, 36)?,
        subtype: u32_at(value, 40)?,
    })
}

/// A file's product version from its version resource, as `major.minor.patch.build` with a
/// trailing `.0` build dropped, or `None` when it has none.
///
/// The product version rather than the file version: it is the one projects bump per release,
/// while some leave the file version at `1.0.0.0`.
#[must_use]
pub fn product_version(bytes: &[u8]) -> Option<String> {
    let parts = parse(&mut &bytes[..])?.version?.fixed?.product_version;
    let shown = if parts[3] == 0 {
        &parts[..3]
    } else {
        &parts[..]
    };
    Some(
        shown
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join("."),
    )
}

#[cfg(test)]
#[path = "pe_tests.rs"]
pub(crate) mod tests;
