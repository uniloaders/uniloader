//! Generates the proxy's export table from `exports/<dll>.txt`.
//!
//! Every file lists the named exports of one system DLL the loader can stand in for. The DLL
//! exports the union of all of them, each a naked jump through one table slot, and fills the
//! slots at load time from whichever system DLL its own file name says it replaces. Adding a
//! DLL is adding a file; see `src/win/proxy.rs`.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

/// Names the loader defines itself, which a list must never shadow.
const RESERVED: &[&str] = &["DllMain"];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = Path::new("exports");
    println!("cargo:rerun-if-changed=exports");
    let mut proxies: Vec<(String, Vec<String>)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "txt") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let names: Vec<String> = std::fs::read_to_string(&path)?
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter(|l| !RESERVED.contains(l))
            .map(str::to_owned)
            .collect();
        for name in &names {
            if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(format!("{}: not an identifier: {name}", path.display()).into());
            }
        }
        proxies.push((stem.to_ascii_lowercase(), names));
    }
    proxies.sort();
    let all: BTreeSet<&str> = proxies
        .iter()
        .flat_map(|(_, names)| names.iter().map(String::as_str))
        .collect();

    let mut out = String::from("forwards! {\n");
    for (index, name) in all.iter().enumerate() {
        writeln!(out, "    {index} {name}")?;
    }
    out.push_str("}\n\n/// System DLLs the proxy knows, with how many named exports each has.\n");
    out.push_str("const KNOWN: &[(&str, usize)] = &[\n");
    for (dll, names) in &proxies {
        writeln!(out, "    ({dll:?}, {}),", names.len())?;
    }
    out.push_str("];\n");
    let target = Path::new(&std::env::var("OUT_DIR")?).join("forwards.rs");
    std::fs::write(target, out)?;
    Ok(())
}
