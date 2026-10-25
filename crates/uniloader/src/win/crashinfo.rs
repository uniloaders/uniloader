//! What a crash report says in words, written into `info.txt` before the dump: the faulting
//! thread's registers, its stack as module + offset per frame (from the unwind tables of x64
//! code), and every module loaded in the process, marked when it belongs to neither Windows
//! nor the game (an overlay's hook DLL, as a rule). A dump that is never finished (the game
//! ended while it was written: a friend's Windows game, 2026-10-25) leaves these lines as the
//! report, readable without a debugger.
//!
//! The module list comes from psapi, which reads the loader's list without taking the loader
//! lock (a crashing thread may hold it); the walk runs under the fault guard, since a
//! corrupted stack can point anywhere.

// FFI module: the process's module list and the stack walk.
#![allow(unsafe_code)]

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Diagnostics::Debug::CONTEXT;
use windows::Win32::System::ProcessStatus::{
    EnumProcessModules, GetModuleFileNameExW, GetModuleInformation, MODULEINFO,
};
use windows::Win32::System::Threading::GetCurrentProcess;

/// Frames walked at most: an overflowed stack holds tens of thousands.
const MAX_FRAMES: usize = 200_000;
/// Frames shown from each end of the stack.
const SHOWN: usize = 48;
/// Modules listed at most.
const MAX_MODULES: usize = 2048;
/// Reads of the module list before giving up.
const ENUM_TRIES: usize = 5;
/// Folders above the game's executable that still count as the game's (UE keeps its own
/// DLLs in `<game>/Engine/Binaries/ThirdParty`, three levels above `Binaries/Win64`).
const GAME_LEVELS: usize = 3;

/// One loaded module.
pub(super) struct Module {
    pub(super) start: usize,
    pub(super) size: usize,
    pub(super) path: PathBuf,
}

impl Module {
    fn name(&self) -> String {
        self.path
            .file_name()
            .map_or_else(|| "?".to_owned(), |n| n.to_string_lossy().into_owned())
    }
}

/// The modules loaded in this process, in load order.
pub(super) fn modules() -> Vec<Module> {
    // SAFETY: a pseudo handle of this process, no reference to release.
    let process = unsafe { GetCurrentProcess() };
    let mut handles = vec![HMODULE::default(); MAX_MODULES];
    let mut needed = 0u32;
    let bytes = u32::try_from(handles.len() * size_of::<HMODULE>()).unwrap_or(u32::MAX);
    // The list is read without the loader lock, so a module loading meanwhile can fail one
    // read (`ERROR_PARTIAL_COPY`); the next one sees it whole.
    let listed = (0..ENUM_TRIES).any(|_| {
        // SAFETY: `handles` is writable for `bytes`; `needed` receives the size used.
        unsafe { EnumProcessModules(process, handles.as_mut_ptr(), bytes, &raw mut needed) }.is_ok()
    });
    if !listed {
        return Vec::new();
    }
    let count = (needed as usize / size_of::<HMODULE>()).min(handles.len());
    handles
        .into_iter()
        .take(count)
        .filter_map(|module| {
            let mut info = MODULEINFO::default();
            // SAFETY: a module handle this process listed; `info` is writable.
            unsafe {
                GetModuleInformation(
                    process,
                    module,
                    &raw mut info,
                    u32::try_from(size_of::<MODULEINFO>()).unwrap_or(0),
                )
            }
            .ok()?;
            let mut buf = [0u16; 1024];
            // SAFETY: `buf` is writable for its length.
            let len = unsafe { GetModuleFileNameExW(Some(process), Some(module), &mut buf) };
            let len = (len as usize).min(buf.len());
            Some(Module {
                start: info.lpBaseOfDll as usize,
                size: info.SizeOfImage as usize,
                path: PathBuf::from(String::from_utf16_lossy(&buf[..len])),
            })
        })
        .collect()
}

/// `address` as `name+0xoffset` in `modules`, or the bare address.
pub(super) fn named(modules: &[Module], address: u64) -> String {
    let at = usize::try_from(address).unwrap_or(0);
    modules
        .iter()
        .find(|m| (m.start..m.start + m.size).contains(&at))
        .map_or_else(
            || format!("{address:#x}"),
            |m| format!("{}+{:#x}", m.name(), at - m.start),
        )
}

/// The faulting thread's registers.
pub(super) fn registers(c: &CONTEXT) -> String {
    format!(
        "registers:\n  rip {:016x}  rsp {:016x}  rbp {:016x}  eflags {:08x}\n  \
         rax {:016x}  rbx {:016x}  rcx {:016x}  rdx {:016x}\n  \
         rsi {:016x}  rdi {:016x}  r8  {:016x}  r9  {:016x}\n  \
         r10 {:016x}  r11 {:016x}  r12 {:016x}  r13 {:016x}\n  \
         r14 {:016x}  r15 {:016x}\n",
        c.Rip,
        c.Rsp,
        c.Rbp,
        c.EFlags,
        c.Rax,
        c.Rbx,
        c.Rcx,
        c.Rdx,
        c.Rsi,
        c.Rdi,
        c.R8,
        c.R9,
        c.R10,
        c.R11,
        c.R12,
        c.R13,
        c.R14,
        c.R15
    )
}

/// The stack of the thread whose state `context` holds, innermost frame first: the first and
/// the last [`SHOWN`] frames (the outermost ones say who started a runaway recursion).
pub(super) fn stack(context: &CONTEXT, modules: &[Module]) -> String {
    let mut first = Vec::with_capacity(SHOWN);
    let mut last = VecDeque::with_capacity(SHOWN);
    let mut total = 0usize;
    let mut context = *context;
    // SAFETY: the walk reads the stack of a thread stopped in a handler; a fault on a
    // corrupted stack is contained and ends the walk.
    let walked = unsafe {
        super::guard::call(|| {
            while total < MAX_FRAMES && context.Rip != 0 {
                if first.len() < SHOWN {
                    first.push(context.Rip);
                } else {
                    if last.len() == SHOWN {
                        last.pop_front();
                    }
                    last.push_back(context.Rip);
                }
                total += 1;
                let rsp = context.Rsp;
                if !super::suspects::unwind(&mut context) || context.Rsp <= rsp {
                    break;
                }
            }
        })
    };
    let mut text = format!("stack ({total} frames):\n");
    frame_lines(&mut text, 0, &first, modules);
    let skipped = total - first.len() - last.len();
    if skipped > 0 {
        let _ = writeln!(text, "  ... {skipped} frames ...");
    }
    let last: Vec<u64> = last.into_iter().collect();
    frame_lines(&mut text, total - last.len(), &last, modules);
    if walked.is_err() {
        text.push_str("  (the walk faulted here: the stack is corrupted)\n");
    }
    text
}

/// One line per frame of `frames` (numbered from `from`); a run of the same frame (a
/// recursion) is one line with its count.
fn frame_lines(text: &mut String, from: usize, frames: &[u64], modules: &[Module]) {
    let mut at = 0;
    while at < frames.len() {
        let run = frames[at..]
            .iter()
            .take_while(|f| **f == frames[at])
            .count();
        let place = named(modules, frames[at]);
        let n = from + at;
        if run == 1 {
            let _ = writeln!(text, "  #{n:<6} {place}");
        } else {
            let _ = writeln!(text, "  #{n:<6} {place}  (x{run}, to #{})", n + run - 1);
        }
        at += run;
    }
}

/// Folders whose modules are Windows' or the game's.
fn own_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    let game = super::proxy::game_dir();
    roots.push(
        game.ancestors()
            .nth(GAME_LEVELS)
            .filter(|a| a.parent().is_some())
            .unwrap_or(&game)
            .to_path_buf(),
    );
    roots
}

fn starts_with_ignoring_case(path: &Path, root: &Path) -> bool {
    let (path, root) = (
        path.to_string_lossy().to_lowercase(),
        root.to_string_lossy().to_lowercase(),
    );
    !root.is_empty() && path.starts_with(&root)
}

/// Whether `module` comes from neither Windows nor the game.
fn is_other(module: &Module, roots: &[PathBuf]) -> bool {
    !roots
        .iter()
        .any(|r| starts_with_ignoring_case(&module.path, r))
}

/// The modules of other programs in the process (overlays, recorders, injected hooks).
pub(super) fn other_programs(modules: &[Module]) -> Vec<String> {
    let roots = own_roots();
    modules
        .iter()
        .filter(|m| is_other(m, &roots))
        .map(|m| shown(&m.path))
        .collect()
}

/// Every module, base and size; `*` marks those of other programs.
pub(super) fn module_list(modules: &[Module]) -> String {
    let roots = own_roots();
    let mut text = format!(
        "modules ({}; * = neither Windows' nor the game's):\n",
        modules.len()
    );
    for m in modules {
        let mark = if is_other(m, &roots) { '*' } else { ' ' };
        let _ = writeln!(
            text,
            "  {mark} {:016x} {:>9x} {}",
            m.start,
            m.size,
            shown(&m.path)
        );
    }
    text
}

/// `path` with the user's profile folder (it holds the account name) as `%USERPROFILE%`.
pub(super) fn shown(path: &Path) -> String {
    shown_text(&path.to_string_lossy())
}

/// `text` with every mention of the user's profile folder as `%USERPROFILE%`.
pub(super) fn shown_text(text: &str) -> String {
    let Some(profile) = std::env::var_os("USERPROFILE").map(|p| p.to_string_lossy().into_owned())
    else {
        return text.to_owned();
    };
    if profile.len() < 4 {
        return text.to_owned();
    }
    let lower = text.to_lowercase();
    let needle = profile.to_lowercase();
    // Lower-casing keeps the byte offsets of ASCII paths; anything else is left as it is.
    if lower.len() != text.len() {
        return text.replace(&profile, "%USERPROFILE%");
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(found) = lower[at..].find(&needle) {
        out.push_str(&text[at..at + found]);
        out.push_str("%USERPROFILE%");
        at += found + needle.len();
    }
    out.push_str(&text[at..]);
    out
}
