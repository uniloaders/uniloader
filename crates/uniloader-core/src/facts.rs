//! The running game's properties, for target filters: what its executable says about itself
//! and what lies around it.
//!
//! Property names, grouped by where they come from (the site's
//! `design/uniloader-plugin-metadata.md` has the full list with sources):
//!
//! - `exe.name`, `exe.path` (with `/`), `exe.folder`, `game.folder` (the folder under
//!   `steamapps/common`, else the executable's folder), `file.size`, `file.sha256`.
//! - `pe.machine`, `pe.bitness`, `pe.timestamp`, `pe.characteristics`, `pe.is_dll`,
//!   `pe.linker_version`, `pe.os_version`, `pe.image_version`, `pe.subsystem_version`,
//!   `pe.subsystem`, `pe.dll_characteristics`, `pe.checksum`, `pe.size_of_image`,
//!   `pe.entry_point`, `pe.signed`; `pe.debug.pdb_guid`, `pe.debug.pdb_age`,
//!   `pe.debug.pdb_path`, `pe.debug.build_id`.
//! - `pe.version.<Key>` for every string of the table a reader would pick, custom keys
//!   included, and `pe.version.<langcp>.<Key>` for every table; `pe.version.translations`,
//!   `pe.version.language`; `pe.fixed.file_version`, `pe.fixed.product_version`,
//!   `pe.fixed.flags`, `pe.fixed.os`, `pe.fixed.type`, `pe.fixed.subtype`.
//! - `dotnet.is_managed`, `dotnet.runtime_version`, `dotnet.flags`.
//! - `unreal.engine_major`, `unreal.branch`, `unreal.changelist`, `unreal.version`,
//!   `unreal.project`; `unity.version`, `unity.backend`.
//! - `steam.app_id`, `steam.build_id`.

use std::path::{Component, Path, PathBuf};

use crate::pe::{self, Image, Reader};
use crate::target::{Fact, Facts};

fn text(facts: &mut Facts, name: &str, value: impl Into<String>) {
    facts.insert(name.to_owned(), Fact::Text(value.into()));
}

fn number(facts: &mut Facts, name: &str, value: impl Into<u64>) {
    facts.insert(name.to_owned(), Fact::Number(value.into()));
}

fn version(facts: &mut Facts, name: &str, parts: &[u16]) {
    facts.insert(
        name.to_owned(),
        Fact::Version(parts.iter().map(|&p| u64::from(p)).collect()),
    );
}

fn flag(facts: &mut Facts, name: &str, value: bool) {
    text(facts, name, if value { "true" } else { "false" });
}

/// Everything readable about the executable at `exe`. `hash` adds `file.sha256`, which reads
/// the whole file (a fraction of a second for a 90 MB game), so it is only asked for when a
/// plugin filters on it.
#[must_use]
pub fn gather(exe: &Path, hash: bool) -> Facts {
    let mut facts = Facts::new();
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    text(&mut facts, "exe.name", name(exe));
    text(
        &mut facts,
        "exe.path",
        exe.to_string_lossy().replace('\\', "/"),
    );
    let folder = exe.parent().map(name).unwrap_or_default();
    text(&mut facts, "exe.folder", folder.clone());
    let steam = steam_install(exe);
    let game_folder = steam.as_ref().map_or(folder, |(_, dir)| dir.clone());
    text(&mut facts, "game.folder", game_folder);
    if let Ok(meta) = std::fs::metadata(exe) {
        number(&mut facts, "file.size", meta.len());
    }
    if hash {
        if let Some(digest) = sha256(exe) {
            text(&mut facts, "file.sha256", digest);
        }
    }
    let image = std::fs::File::open(exe)
        .ok()
        .and_then(|f| pe::parse(&mut Reader(std::io::BufReader::new(f))));
    if let Some(image) = &image {
        image_facts(&mut facts, image);
        unreal_facts(&mut facts, exe, image);
    }
    unity_facts(&mut facts, exe);
    if let Some((steamapps, dir)) = steam {
        steam_facts(&mut facts, &steamapps, &dir);
    }
    if !facts.contains_key("steam.app_id") {
        let file = exe.with_file_name("steam_appid.txt");
        if let Some(id) = std::fs::read_to_string(file)
            .ok()
            .and_then(|t| t.trim().parse::<u64>().ok())
        {
            number(&mut facts, "steam.app_id", id);
        }
    }
    facts
}

/// The file's SHA-256, lower-case hex.
#[must_use]
pub fn sha256(file: &Path) -> Option<String> {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    let mut f = std::fs::File::open(file).ok()?;
    std::io::copy(&mut f, &mut hasher).ok()?;
    Some(pe::hex(&hasher.finalize()))
}

fn image_facts(facts: &mut Facts, image: &Image) {
    number(facts, "pe.machine", image.machine);
    number(facts, "pe.bitness", image.bitness);
    number(facts, "pe.timestamp", image.timestamp);
    number(facts, "pe.characteristics", image.characteristics);
    flag(facts, "pe.is_dll", image.characteristics & 0x2000 != 0);
    version(facts, "pe.linker_version", &image.linker_version);
    version(facts, "pe.os_version", &image.os_version);
    version(facts, "pe.image_version", &image.image_version);
    version(facts, "pe.subsystem_version", &image.subsystem_version);
    number(facts, "pe.subsystem", image.subsystem);
    number(facts, "pe.dll_characteristics", image.dll_characteristics);
    number(facts, "pe.checksum", image.checksum);
    number(facts, "pe.size_of_image", image.size_of_image);
    number(facts, "pe.entry_point", image.entry_point);
    flag(facts, "pe.signed", image.signed);
    if let Some(pdb) = &image.pdb {
        text(facts, "pe.debug.pdb_guid", pdb.guid.clone());
        number(facts, "pe.debug.pdb_age", pdb.age);
        text(facts, "pe.debug.pdb_path", pdb.path.clone());
        text(facts, "pe.debug.build_id", pdb.build_id.clone());
    }
    flag(facts, "dotnet.is_managed", image.clr.is_some());
    if let Some(clr) = &image.clr {
        text(facts, "dotnet.runtime_version", clr.runtime_version.clone());
        number(facts, "dotnet.flags", clr.flags);
    }
    let Some(info) = &image.version else {
        return;
    };
    number(facts, "pe.version.language", info.language);
    let translations: Vec<String> = info
        .translations
        .iter()
        .map(|(lang, cp)| format!("{lang:04x}{cp:04x}"))
        .collect();
    text(facts, "pe.version.translations", translations.join(" "));
    for (key, table) in &info.tables {
        for (name, value) in table {
            text(facts, &format!("pe.version.{key}.{name}"), value.clone());
        }
    }
    for (name, value) in info.table().unwrap_or_default() {
        text(facts, &format!("pe.version.{name}"), value.clone());
    }
    if let Some(fixed) = &info.fixed {
        version(facts, "pe.fixed.file_version", &fixed.file_version);
        version(facts, "pe.fixed.product_version", &fixed.product_version);
        number(facts, "pe.fixed.flags", fixed.flags);
        number(facts, "pe.fixed.os", fixed.os);
        number(facts, "pe.fixed.type", fixed.file_type);
        number(facts, "pe.fixed.subtype", fixed.subtype);
    }
}

/// Unreal's build string, `++UE4+Release-4.26-CL-0`: engine major, branch, changelist.
fn unreal_build(build: &str) -> Option<(u64, String, Option<u64>)> {
    let rest = build.strip_prefix("++UE")?;
    let (major, branch) = rest.split_once('+')?;
    let major = major.parse().ok()?;
    let (branch, changelist) = match branch.rsplit_once("-CL-") {
        Some((branch, cl)) => (branch, cl.parse().ok()),
        None => (branch, None),
    };
    Some((major, branch.to_owned(), changelist))
}

fn unreal_facts(facts: &mut Facts, exe: &Path, image: &Image) {
    let info = image.version.as_ref();
    let build = info
        .and_then(|i| i.string("ProductVersion"))
        .and_then(unreal_build);
    // A packaged game: `<root>/<Project>/Binaries/Win64/<exe>`, with `<root>/Engine` beside.
    let win64 = exe.parent();
    let binaries = win64.and_then(Path::parent);
    let project_dir = binaries
        .filter(|b| {
            b.file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("Binaries"))
        })
        .and_then(Path::parent);
    let layout = project_dir
        .and_then(Path::parent)
        .is_some_and(|root| root.join("Engine").is_dir());
    if build.is_none() && !layout {
        return;
    }
    if let Some((major, branch, changelist)) = build {
        number(facts, "unreal.engine_major", major);
        text(facts, "unreal.branch", branch);
        if let Some(cl) = changelist {
            number(facts, "unreal.changelist", cl);
        }
        // The resource script writes the engine version as the file version.
        if let Some(fixed) = info.and_then(|i| i.fixed) {
            version(facts, "unreal.version", &fixed.file_version[..3]);
        }
    }
    let project = project_dir
        .filter(|_| layout)
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .or_else(|| {
            info.and_then(|i| i.string("InternalName"))
                .map(str::to_owned)
        });
    if let Some(project) = project {
        text(facts, "unreal.project", project);
    }
}

fn unity_facts(facts: &mut Facts, exe: &Path) {
    let Some(dir) = exe.parent() else {
        return;
    };
    let player = dir.join("UnityPlayer.dll");
    let version = std::fs::File::open(&player)
        .ok()
        .and_then(|f| pe::parse(&mut Reader(std::io::BufReader::new(f))))
        .and_then(|i| i.version)
        .and_then(|v| v.string("ProductVersion").map(str::to_owned));
    if let Some(version) = version {
        // `2021.3.16f1 (4016570cf34f)`: the part before the hash.
        let version = version.split_whitespace().next().unwrap_or_default();
        text(facts, "unity.version", version);
    }
    let stem = exe
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if dir.join("GameAssembly.dll").is_file() {
        text(facts, "unity.backend", "il2cpp");
    } else if dir.join(format!("{stem}_Data")).join("Managed").is_dir() {
        text(facts, "unity.backend", "mono");
    }
}

/// `steamapps` and the install folder name, when `exe` is under `steamapps/common/<name>/`.
fn steam_install(exe: &Path) -> Option<(PathBuf, String)> {
    let parts: Vec<Component<'_>> = exe.components().collect();
    let at = parts.windows(2).position(|w| {
        w[0].as_os_str().eq_ignore_ascii_case("steamapps")
            && w[1].as_os_str().eq_ignore_ascii_case("common")
    })?;
    let folder = parts
        .get(at + 2)?
        .as_os_str()
        .to_string_lossy()
        .into_owned();
    let steamapps: PathBuf = parts[..=at].iter().collect();
    Some((steamapps, folder))
}

/// The value of `"key"` in a Valve `KeyValues` text, from its first occurrence.
fn acf_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let mut quoted = line.split('"').skip(1).step_by(2);
        (quoted.next()?.eq_ignore_ascii_case(key)).then(|| quoted.next())?
    })
}

fn steam_facts(facts: &mut Facts, steamapps: &Path, folder: &str) {
    let Ok(entries) = std::fs::read_dir(steamapps) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        let acf = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("acf"));
        if !(acf && name.starts_with("appmanifest_")) {
            continue;
        }
        let Ok(manifest) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !acf_value(&manifest, "installdir").is_some_and(|d| d.eq_ignore_ascii_case(folder)) {
            continue;
        }
        for (key, fact) in [("appid", "steam.app_id"), ("buildid", "steam.build_id")] {
            if let Some(value) = acf_value(&manifest, key).and_then(|v| v.parse::<u64>().ok()) {
                number(facts, fact, value);
            }
        }
        return;
    }
}

#[cfg(test)]
#[path = "facts_tests.rs"]
mod tests;
