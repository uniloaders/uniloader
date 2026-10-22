use super::*;

fn utf16z(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn pad4(bytes: &mut Vec<u8>) {
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
}

/// One `VS_VERSIONINFO` node: header, key, value, children, as a resource compiler writes it.
fn vnode(key: &str, value: &[u8], text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let units = if text { value.len() / 2 } else { value.len() };
    let mut out = vec![0, 0];
    out.extend_from_slice(&u16::try_from(units).unwrap_or_default().to_le_bytes());
    out.extend_from_slice(&u16::from(text).to_le_bytes());
    out.extend(utf16z(key));
    pad4(&mut out);
    out.extend_from_slice(value);
    for child in children {
        pad4(&mut out);
        out.extend_from_slice(child);
    }
    let len = u16::try_from(out.len()).unwrap_or_default();
    out[..2].copy_from_slice(&len.to_le_bytes());
    pad4(&mut out);
    out
}

fn fixed_info(file: [u16; 4], product: [u16; 4]) -> Vec<u8> {
    let join = |v: [u16; 4]| {
        [
            (u32::from(v[0]) << 16) | u32::from(v[1]),
            (u32::from(v[2]) << 16) | u32::from(v[3]),
        ]
    };
    let [fms, fls] = join(file);
    let [pms, pls] = join(product);
    [
        0xFEEF_04BD,
        0x0001_0000,
        fms,
        fls,
        pms,
        pls,
        0x3F,
        0x2,
        0x0004_0004,
        2,
        0,
        0,
        0,
    ]
    .iter()
    .flat_map(|v: &u32| v.to_le_bytes())
    .collect()
}

/// A version resource like Train Sim World's: no `FileVersion` string, Unreal's
/// `ProductVersion`, one table `040904b0`.
pub(crate) fn tsw_version() -> Vec<u8> {
    let s = |k: &str, v: &str| vnode(k, &utf16z(v), true, &[]);
    let table = vnode(
        "040904b0",
        &[],
        true,
        &[
            s("CompanyName", "Dovetail Games"),
            s("ProductName", "TrainSimWorld7"),
            s("ProductVersion", "++UE4+Release-4.26-CL-0"),
            s("InternalName", "TS2Prototype"),
            s("Custom Key", "x"),
        ],
    );
    let strings = vnode("StringFileInfo", &[], true, &[table]);
    let translation = vnode("Translation", &[0x09, 0x04, 0xB0, 0x04], false, &[]);
    let vars = vnode("VarFileInfo", &[], true, &[translation]);
    vnode(
        "VS_VERSION_INFO",
        &fixed_info([4, 26, 1, 0], [4, 26, 1, 0]),
        false,
        &[strings, vars],
    )
}

fn put(bytes: &mut [u8], at: usize, value: &[u8]) {
    bytes[at..at + value.len()].copy_from_slice(value);
}

/// A minimal 64-bit image: one section at RVA 0x1000 (file 0x400) holding the resource
/// directory with `version`, a debug directory and its `CodeView` record.
pub(crate) fn image(version: &[u8]) -> Vec<u8> {
    const RVA: u32 = 0x1000;
    let mut section = vec![0u8; 0x60];
    // Root: one ID entry, RT_VERSION -> subdirectory at 0x18.
    put(&mut section, 14, &1u16.to_le_bytes());
    put(&mut section, 16, &16u32.to_le_bytes());
    put(&mut section, 20, &(0x8000_0000u32 | 0x18).to_le_bytes());
    // Names: ID 1 -> subdirectory at 0x30.
    put(&mut section, 0x18 + 14, &1u16.to_le_bytes());
    put(&mut section, 0x18 + 16, &1u32.to_le_bytes());
    put(
        &mut section,
        0x18 + 20,
        &(0x8000_0000u32 | 0x30).to_le_bytes(),
    );
    // Languages: 0x409 -> data entry at 0x50.
    put(&mut section, 0x30 + 14, &1u16.to_le_bytes());
    put(&mut section, 0x30 + 16, &0x409u32.to_le_bytes());
    put(&mut section, 0x30 + 20, &0x50u32.to_le_bytes());
    let data_rva = RVA + 0x60;
    put(&mut section, 0x50, &data_rva.to_le_bytes());
    put(
        &mut section,
        0x54,
        &u32::try_from(version.len())
            .unwrap_or_default()
            .to_le_bytes(),
    );
    section.extend_from_slice(version);
    pad4(&mut section);
    let resource_size = u32::try_from(section.len()).unwrap_or_default();

    let codeview_rva = RVA + resource_size + 28;
    let mut codeview = b"RSDS".to_vec();
    codeview.extend_from_slice(&0x5096_FE56u32.to_le_bytes());
    codeview.extend_from_slice(&0x872Fu16.to_le_bytes());
    codeview.extend_from_slice(&0x403Bu16.to_le_bytes());
    codeview.extend_from_slice(&[0xB2, 0x9F, 0x77, 0xD1, 0x6A, 0x0D, 0xCB, 0x63]);
    codeview.extend_from_slice(&1u32.to_le_bytes());
    codeview.extend_from_slice(b"TrainSimWorld.pdb\0");
    let mut debug = vec![0u8; 28];
    put(&mut debug, 12, &2u32.to_le_bytes());
    let codeview_len = u32::try_from(codeview.len()).unwrap_or_default();
    put(&mut debug, 16, &codeview_len.to_le_bytes());
    put(&mut debug, 20, &codeview_rva.to_le_bytes());
    section.extend_from_slice(&debug);
    section.extend_from_slice(&codeview);
    let section_len = u32::try_from(section.len()).unwrap_or_default();

    let mut file = vec![0u8; 0x400];
    file[..2].copy_from_slice(b"MZ");
    put(&mut file, 0x3C, &0x80u32.to_le_bytes());
    put(&mut file, 0x80, b"PE\0\0");
    let coff = 0x84;
    put(&mut file, coff, &0x8664u16.to_le_bytes());
    put(&mut file, coff + 2, &1u16.to_le_bytes());
    put(&mut file, coff + 4, &0x6ACD_35D0u32.to_le_bytes());
    put(&mut file, coff + 16, &240u16.to_le_bytes());
    put(&mut file, coff + 18, &0x22u16.to_le_bytes());
    let opt = coff + 20;
    put(&mut file, opt, &0x20Bu16.to_le_bytes());
    file[opt + 2] = 14;
    file[opt + 3] = 29;
    put(&mut file, opt + 40, &6u16.to_le_bytes());
    put(&mut file, opt + 68, &2u16.to_le_bytes());
    put(&mut file, opt + 70, &0x8160u16.to_le_bytes());
    put(&mut file, opt + 108, &16u32.to_le_bytes());
    // Data directories: 2 resources, 6 debug.
    put(&mut file, opt + 112 + 2 * 8, &RVA.to_le_bytes());
    put(
        &mut file,
        opt + 112 + 2 * 8 + 4,
        &resource_size.to_le_bytes(),
    );
    put(
        &mut file,
        opt + 112 + 6 * 8,
        &(RVA + resource_size).to_le_bytes(),
    );
    put(&mut file, opt + 112 + 6 * 8 + 4, &28u32.to_le_bytes());
    let table = opt + 240;
    put(&mut file, table, b".rsrc\0\0\0");
    put(&mut file, table + 8, &section_len.to_le_bytes());
    put(&mut file, table + 12, &RVA.to_le_bytes());
    put(&mut file, table + 16, &section_len.to_le_bytes());
    put(&mut file, table + 20, &0x400u32.to_le_bytes());
    file.extend_from_slice(&section);
    file
}

#[test]
fn reads_headers_debug_record_and_version() {
    let bytes = image(&tsw_version());
    let image = parse(&mut &bytes[..]).unwrap_or_else(|| panic!("a PE image"));
    assert_eq!(image.machine, 0x8664);
    assert_eq!(image.bitness, 64);
    assert_eq!(image.timestamp, 0x6ACD_35D0);
    assert_eq!(image.characteristics, 0x22);
    assert_eq!(image.linker_version, [14, 29]);
    assert_eq!(image.os_version, [6, 0]);
    assert_eq!(image.subsystem, 2);
    assert_eq!(image.dll_characteristics, 0x8160);
    assert!(!image.signed);
    assert_eq!(image.clr, None);
    let pdb = image.pdb.unwrap_or_else(|| panic!("a CodeView record"));
    assert_eq!(pdb.guid, "5096fe56-872f-403b-b29f-77d16a0dcb63");
    assert_eq!(pdb.build_id, "5096FE56872F403BB29F77D16A0DCB631");
    assert_eq!(pdb.path, "TrainSimWorld.pdb");
    let version = image
        .version
        .unwrap_or_else(|| panic!("a version resource"));
    assert_eq!(version.language, 0x409);
    assert_eq!(version.translations, vec![(0x409, 0x4B0)]);
    let fixed = version.fixed.unwrap_or_else(|| panic!("VS_FIXEDFILEINFO"));
    assert_eq!(fixed.file_version, [4, 26, 1, 0]);
    assert_eq!(fixed.flags, 0x2);
    assert_eq!(fixed.file_type, 2);
    assert_eq!(version.string("productname"), Some("TrainSimWorld7"));
    assert_eq!(version.string("Custom Key"), Some("x"));
    assert_eq!(version.string("FileVersion"), None);
    assert_eq!(version.tables[0].0, "040904b0");
}

#[test]
fn reads_through_a_seekable_reader_too() {
    let bytes = image(&tsw_version());
    let mut reader = Reader(std::io::Cursor::new(bytes));
    let image = parse(&mut reader).unwrap_or_else(|| panic!("a PE image"));
    assert_eq!(
        image
            .version
            .and_then(|v| v.string("CompanyName").map(str::to_owned)),
        Some("Dovetail Games".to_owned())
    );
}

#[test]
fn product_version_drops_a_zero_build() {
    let resource = vnode(
        "VS_VERSION_INFO",
        &fixed_info([1, 0, 0, 0], [3, 0, 1, 0]),
        false,
        &[],
    );
    assert_eq!(product_version(&image(&resource)).as_deref(), Some("3.0.1"));
    let resource = vnode(
        "VS_VERSION_INFO",
        &fixed_info([1, 0, 0, 0], [2, 5, 0, 7]),
        false,
        &[],
    );
    assert_eq!(
        product_version(&image(&resource)).as_deref(),
        Some("2.5.0.7")
    );
}

#[test]
fn not_an_image_or_no_resource_is_none() {
    assert_eq!(parse(&mut &[0u8; 256][..]), None);
    assert_eq!(product_version(&[]), None);
    let mut broken = image(&tsw_version());
    broken[0x80] = b'X';
    assert_eq!(parse(&mut &broken[..]), None);
    // A resource that is not a version resource leaves only that part empty.
    let image = parse(&mut &image(&[0u8; 64])[..]).unwrap_or_else(|| panic!("headers still parse"));
    assert_eq!(image.version, None);
    assert!(image.pdb.is_some());
}

#[test]
fn table_selection_prefers_translations_then_us_english() {
    let table = |key: &str, value: &str| (key.to_owned(), vec![("K".to_owned(), value.to_owned())]);
    let mut info = VersionInfo {
        tables: vec![table("040704b0", "de"), table("040904b0", "en")],
        ..VersionInfo::default()
    };
    assert_eq!(info.string("k"), Some("en"));
    info.translations = vec![(0x407, 0x4B0)];
    assert_eq!(info.string("k"), Some("de"));
    info.tables.remove(1);
    info.translations.clear();
    assert_eq!(
        info.string("k"),
        Some("de"),
        "the first table as the last resort"
    );
}
