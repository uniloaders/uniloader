//! Inline function detours: the jump written at a function's entry, and the trampoline that
//! still reaches the original.
//!
//! The shape is the usual one. The target's first instructions are decoded until at least a
//! five byte `jmp rel32` fits, copied into a trampoline that ends in an absolute jump back to
//! the rest of the function, and the entry is replaced with a jump to the plugin. Because a
//! plugin DLL can sit further than two gigabytes from the game's code, the five byte jump goes
//! to a relay allocated *near the target*, and the relay performs the 14 byte absolute jump.
//!
//! An entry that already begins with a `jmp rel32` or `jmp qword [rip+x]` is another
//! program's hook, as a rule (an overlay's), and is chained behind the way the Steam overlay
//! does it: only that jump is stolen, and the trampoline goes where it went. A jump through a
//! pointer stays a jump through that same pointer, so the other program can still repoint it.
//!
//! What is deliberately refused rather than guessed:
//!
//! - an instruction in the stolen bytes that is relative to the instruction pointer, which
//!   would mean something different once it lives in the trampoline;
//! - any other branch, call or return in the stolen bytes, for the same reason;
//! - a prologue that does not reach five bytes within a handful of instructions.
//!
//! What this does not do is suspend the game's other threads while the jump is written. A
//! thread executing exactly those bytes in that instant is a real if small risk, so hooks
//! belong in `start`, not in the middle of whatever the hooked code is busy with.

// FFI module: allocates executable memory and rewrites the game's code.
#![allow(unsafe_code)]

use core::ffi::c_void;

use iced_x86::{Decoder, DecoderOptions, FlowControl};
use uniloader_api::Status;
use windows::Win32::System::Memory::{
    VirtualAlloc, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
};
use windows::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};

/// Bytes of a `jmp rel32`.
const JUMP_LEN: usize = 5;
/// Bytes of a `jmp qword [rip+0]` followed by the absolute address.
const ABSOLUTE_JUMP_LEN: usize = 14;
/// How many instructions to consider before giving up on a prologue.
const MAX_INSTRUCTIONS: usize = 8;

/// The most entry bytes [`install`] reads: [`MAX_INSTRUCTIONS`] of the longest x86 length.
pub(super) const MAX_ENTRY_READ: usize = MAX_INSTRUCTIONS * 16;
/// How far from the target to look for a place to put the relay.
const NEAR_RANGE: usize = 512 * 1024 * 1024;

/// An installed detour. Removing it is writing `original` back over the entry.
pub(super) struct Detour {
    /// Function that was detoured.
    pub(super) target: usize,
    /// The entry bytes as they were.
    pub(super) original: Vec<u8>,
    /// Address a plugin calls to reach the original function.
    pub(super) trampoline: *mut c_void,
}

/// Decode the entry until at least [`JUMP_LEN`] bytes are covered by whole instructions.
///
/// Returns how many bytes to steal, or why they cannot be.
fn stolen_length(code: &[u8], address: u64) -> Result<usize, String> {
    let mut decoder = Decoder::with_ip(64, code, address, DecoderOptions::NONE);
    let mut length = 0usize;
    for _ in 0..MAX_INSTRUCTIONS {
        if length >= JUMP_LEN {
            return Ok(length);
        }
        if !decoder.can_decode() {
            break;
        }
        let instruction = decoder.decode();
        if instruction.is_invalid() {
            return Err(format!(
                "undecodable instruction at {:#X}",
                instruction.ip()
            ));
        }
        if instruction.is_ip_rel_memory_operand() {
            return Err(format!(
                "instruction at {:#X} is relative to the instruction pointer and cannot be moved",
                instruction.ip()
            ));
        }
        if instruction.flow_control() != FlowControl::Next {
            return Err(format!(
                "instruction at {:#X} changes control flow and cannot be moved",
                instruction.ip()
            ));
        }
        length += instruction.len();
    }
    if length >= JUMP_LEN {
        Ok(length)
    } else {
        Err(format!(
            "the first {MAX_INSTRUCTIONS} instructions do not add up to {JUMP_LEN} movable bytes"
        ))
    }
}

/// A jump at a function's entry: another program's hook, as a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LeadingJump {
    /// Where a direct jump goes, or for `jmp qword [rip+x]` the address of the pointer it
    /// jumps through.
    pub(super) to: u64,
    /// `to` is the pointer's address rather than the destination.
    pub(super) through: bool,
    /// Bytes of the jump instruction.
    pub(super) len: usize,
}

/// The jump an entry begins with, if it begins with an unconditional near jump.
pub(super) fn leading_jump(code: &[u8], address: u64) -> Option<LeadingJump> {
    let mut decoder = Decoder::with_ip(64, code, address, DecoderOptions::NONE);
    if !decoder.can_decode() {
        return None;
    }
    let instruction = decoder.decode();
    let (to, through) = match instruction.flow_control() {
        FlowControl::UnconditionalBranch => {
            Some((instruction.near_branch_target(), false)).filter(|(to, _)| *to != 0)?
        }
        FlowControl::IndirectBranch if instruction.is_ip_rel_memory_operand() => {
            (instruction.ip_rel_memory_address(), true)
        }
        _ => return None,
    };
    Some(LeadingJump {
        to,
        through,
        len: instruction.len(),
    })
}

/// What the entry gives up to the jump to the plugin.
#[derive(Debug, PartialEq, Eq)]
enum Stolen {
    /// This many bytes of whole, movable instructions, copied into the trampoline as they are.
    Prologue(usize),
    /// Another program's jump, which the trampoline re-creates.
    Chain(LeadingJump),
}

impl Stolen {
    /// Entry bytes overwritten, and written back on removal.
    fn len(&self) -> usize {
        match self {
            Self::Prologue(len) => *len,
            Self::Chain(jump) => jump.len,
        }
    }
}

/// What to steal from the entry at `address`, or why nothing can be.
///
/// A leading jump is chained only when it alone covers [`JUMP_LEN`]: a `jmp rel8` would need
/// the bytes after it, which are not this function's code.
fn steal(code: &[u8], address: u64) -> Result<Stolen, String> {
    match leading_jump(code, address) {
        Some(jump) if jump.len >= JUMP_LEN => Ok(Stolen::Chain(jump)),
        _ => stolen_length(code, address).map(Stolen::Prologue),
    }
}

/// The `rel32` of an instruction ending at `end` that reaches `to`.
fn rel32(to: usize, end: usize) -> Result<i32, String> {
    // Through `i64` rather than `isize` arithmetic: a user-mode address never exceeds the
    // positive range, and the conversion says so instead of assuming it.
    let reach = |what: usize| i64::try_from(what).map_err(|_| "address out of range".to_owned());
    i32::try_from(reach(to)? - reach(end)?)
        .map_err(|_| format!("{to:#X} is out of reach of {end:#X}"))
}

/// An address from a decoded one, which is a `u64` by the decoder's choice.
fn address(value: u64) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| format!("{value:#X} is not an address"))
}

/// The trampoline's code when it lives at `at`: what the stolen bytes did, then onwards.
fn trampoline_code(
    stolen: &Stolen,
    entry: &[u8],
    target: usize,
    at: usize,
) -> Result<Vec<u8>, String> {
    match *stolen {
        Stolen::Prologue(len) => {
            let mut code = entry.get(..len).ok_or("stolen past the entry")?.to_vec();
            code.extend_from_slice(&absolute_jump(target + len));
            Ok(code)
        }
        Stolen::Chain(LeadingJump {
            to, through: false, ..
        }) => Ok(absolute_jump(address(to)?).to_vec()),
        Stolen::Chain(LeadingJump {
            to: slot,
            through: true,
            ..
        }) => {
            // Read the pointer when the jump runs, as the other program's own jump does.
            let mut code = vec![0xFF, 0x25];
            code.extend_from_slice(&rel32(address(slot)?, at + 6)?.to_ne_bytes());
            Ok(code)
        }
    }
}

/// The bytes written over `len` entry bytes at `target`: `jmp rel32` to the relay, then `nop`
/// over the rest so the entry stays a sequence of whole instructions.
fn entry_patch(target: usize, relay: usize, len: usize) -> Result<Vec<u8>, String> {
    // A `jmp rel32` counts from the end of the instruction, hence `target + JUMP_LEN`.
    let displacement = rel32(relay, target + JUMP_LEN)
        .map_err(|_| format!("the relay at {relay:#X} is out of reach of {target:#X}"))?;
    let mut patch = vec![0x90u8; len];
    patch[0] = 0xE9;
    patch[1..JUMP_LEN].copy_from_slice(&displacement.to_ne_bytes());
    Ok(patch)
}

/// `jmp qword [rip+0]; <address>`: an absolute jump that clobbers no register.
fn absolute_jump(to: usize) -> [u8; ABSOLUTE_JUMP_LEN] {
    let mut bytes = [0u8; ABSOLUTE_JUMP_LEN];
    bytes[0] = 0xFF;
    bytes[1] = 0x25;
    // The displacement is zero: the address follows the instruction.
    bytes[6..].copy_from_slice(&to.to_ne_bytes());
    bytes
}

/// The allocation granularity, which is the step a `VirtualAlloc` hint has to move in.
fn granularity() -> usize {
    let mut info = SYSTEM_INFO::default();
    // SAFETY: `info` is writable; the call only fills it in.
    unsafe { GetSystemInfo(&raw mut info) };
    let value = info.dwAllocationGranularity as usize;
    if value == 0 {
        0x10000
    } else {
        value
    }
}

/// Commit one executable page, as close to `near` as the address space allows.
///
/// Close matters for the relay: a `jmp rel32` only reaches two gigabytes, so the relay has to
/// be within that of the target. A page anywhere is fine for the trampoline itself, which is
/// only ever entered through an absolute jump.
fn alloc_executable(near: Option<usize>) -> Option<*mut u8> {
    let page = 4096;
    let commit = MEM_COMMIT | MEM_RESERVE;
    if let Some(near) = near {
        let step = granularity();
        let base = near - near % step;
        let mut offset = step;
        while offset <= NEAR_RANGE {
            for candidate in [base.checked_sub(offset), base.checked_add(offset)]
                .into_iter()
                .flatten()
            {
                // SAFETY: a hinted allocation either succeeds at a usable address or returns
                // null; nothing is read or written here.
                let got = unsafe {
                    VirtualAlloc(
                        Some(candidate as *const c_void),
                        page,
                        commit,
                        PAGE_EXECUTE_READWRITE,
                    )
                };
                if !got.is_null() {
                    return Some(got.cast::<u8>());
                }
            }
            offset += step;
        }
        return None;
    }
    // SAFETY: an unhinted allocation either succeeds or returns null.
    let got = unsafe { VirtualAlloc(None, page, commit, PAGE_EXECUTE_READWRITE) };
    (!got.is_null()).then(|| got.cast::<u8>())
}

/// Install a detour at `target`.
///
/// The relay and trampoline pages are never freed: another thread may be inside the
/// trampoline at the moment the detour is removed, and one page per hook for the lifetime of
/// a game session costs nothing against having to prove otherwise.
///
/// `ready` receives the trampoline before the jump is written. A replacement that is called
/// the instant the jump exists, on another thread, finds its way to the original only if it
/// was stored by then; returning it afterwards is too late for a function the game calls
/// constantly.
///
/// # Safety
/// `target` must be the entry point of a function, with `readable` bytes of committed memory
/// from it, and `replacement` must have its exact signature and calling convention.
pub(super) unsafe fn install(
    target: usize,
    readable: usize,
    replacement: usize,
    ready: impl FnOnce(*mut c_void),
    write: impl Fn(usize, &[u8]) -> Result<Vec<u8>, Status>,
) -> Result<Detour, String> {
    // The longest prologue the decoder could need, but never past the end of the region.
    let window = readable.min(MAX_ENTRY_READ);
    // SAFETY: the caller established that `readable` bytes from `target` are committed.
    let entry = unsafe { core::slice::from_raw_parts(target as *const u8, window) };
    let stolen = steal(entry, target as u64)?;

    // A jump through a pointer is re-created as one, whose `rel32` must reach the pointer.
    let near = match stolen {
        Stolen::Chain(LeadingJump {
            to, through: true, ..
        }) => Some(address(to)?),
        _ => None,
    };
    let trampoline = alloc_executable(near).ok_or("cannot allocate a trampoline page")?;
    let code = trampoline_code(&stolen, entry, target, trampoline as usize)?;
    let relay = alloc_executable(Some(target)).ok_or_else(|| {
        format!("cannot allocate a relay page within {NEAR_RANGE:#X} of {target:#X}")
    })?;
    let patch = entry_patch(target, relay as usize, stolen.len())?;

    // SAFETY: both pages are freshly committed and writable; the trampoline is at most
    // `MAX_ENTRY_READ + ABSOLUTE_JUMP_LEN` bytes and the relay `ABSOLUTE_JUMP_LEN`, both well
    // under one page.
    unsafe {
        core::ptr::copy_nonoverlapping(code.as_ptr(), trampoline, code.len());
        let to_plugin = absolute_jump(replacement);
        core::ptr::copy_nonoverlapping(to_plugin.as_ptr(), relay, to_plugin.len());
    }

    ready(trampoline.cast::<c_void>());
    let original = write(target, &patch).map_err(|e| format!("cannot write the jump: {e:?}"))?;
    log::debug!("detoured {target:#X} -> {replacement:#X} via relay {relay:p}, stole {stolen:?}");
    Ok(Detour {
        target,
        original,
        trampoline: trampoline.cast::<c_void>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_jump_is_a_rip_relative_jump_to_the_following_address() {
        let bytes = absolute_jump(0x1234_5678_9ABC_DEF0);
        assert_eq!(&bytes[..6], &[0xFF, 0x25, 0, 0, 0, 0]);
        assert_eq!(
            usize::from_ne_bytes(bytes[6..].try_into().unwrap_or([0; 8])),
            0x1234_5678_9ABC_DEF0
        );
    }

    #[test]
    fn a_normal_prologue_is_stolen_whole() {
        // push rbp; mov rbp, rsp; sub rsp, 0x20  (1 + 3 + 4 = 8 bytes)
        let code = [
            0x55, 0x48, 0x89, 0xE5, 0x48, 0x83, 0xEC, 0x20, 0x90, 0x90, 0x90, 0x90,
        ];
        assert_eq!(stolen_length(&code, 0x1000), Ok(8));
    }

    #[test]
    fn five_bytes_of_whole_instructions_is_enough() {
        // mov eax, 1 (5 bytes) exactly.
        let code = [0xB8, 0x01, 0x00, 0x00, 0x00, 0xC3];
        assert_eq!(stolen_length(&code, 0x1000), Ok(5));
    }

    #[test]
    fn relative_and_branching_prologues_are_refused() {
        // lea rax, [rip+0x10]: moving it would change what it points at.
        let rip_relative = [0x48, 0x8D, 0x05, 0x10, 0x00, 0x00, 0x00];
        let refusal = stolen_length(&rip_relative, 0x1000);
        assert!(
            matches!(&refusal, Err(e) if e.contains("relative to the instruction pointer")),
            "{refusal:?}"
        );

        // jmp rel8: too short to chain, and what follows it is not this function's code.
        let short = [0xEB, 0x10, 0x90, 0x90, 0x90, 0x90];
        let refusal = steal(&short, 0x1000);
        assert!(
            matches!(&refusal, Err(e) if e.contains("changes control flow")),
            "{refusal:?}"
        );
        // call rel32 is no hook to chain behind.
        let call = [0xE8, 0x00, 0x10, 0x00, 0x00, 0x90];
        assert!(steal(&call, 0x1000).is_err());
        // A jump after the first instruction is still refused.
        let later = [0x55, 0xE9, 0x00, 0x10, 0x00, 0x00];
        assert!(steal(&later, 0x1000).is_err());

        // ret immediately: nothing to steal.
        let ret = [0xC3, 0x90, 0x90, 0x90, 0x90, 0x90];
        assert!(steal(&ret, 0x1000).is_err());
    }

    #[test]
    fn a_leading_jmp_rel32_is_chained_to_its_target() {
        // jmp rel32 to 0x1005 + 0x1000, then a byte of whatever.
        let jump = [0xE9, 0x00, 0x10, 0x00, 0x00, 0xCC];
        let expected = LeadingJump {
            to: 0x2005,
            through: false,
            len: 5,
        };
        assert_eq!(leading_jump(&jump, 0x1000), Some(expected));
        let stolen = steal(&jump, 0x1000);
        assert_eq!(stolen, Ok(Stolen::Chain(expected)));
        let code = stolen.and_then(|s| trampoline_code(&s, &jump, 0x1000, 0x7000_0000));
        assert_eq!(code, Ok(absolute_jump(0x2005).to_vec()));
    }

    #[test]
    fn a_leading_jmp_through_a_pointer_still_reads_that_pointer() {
        // jmp qword [rip+0x10]: through a pointer at the next instruction + 0x10.
        let through = [0xFF, 0x25, 0x10, 0x00, 0x00, 0x00, 0xCC];
        let expected = LeadingJump {
            to: 0x1016,
            through: true,
            len: 6,
        };
        assert_eq!(leading_jump(&through, 0x1000), Some(expected));
        assert_eq!(leading_jump(&[0x55, 0x48, 0x89, 0xE5], 0x1000), None);
        let stolen = steal(&through, 0x1000);
        assert_eq!(stolen, Ok(Stolen::Chain(expected)));

        // From a trampoline at 0x5000 the same slot is 0x1016 - 0x5006 away.
        let code = stolen.and_then(|s| trampoline_code(&s, &through, 0x1000, 0x5000));
        let mut bytes = vec![0xFF, 0x25];
        bytes.extend_from_slice(&(0x1016i32 - 0x5006).to_ne_bytes());
        assert_eq!(code.as_ref(), Ok(&bytes));
        assert_eq!(
            leading_jump(&bytes, 0x5000).map(|j| (j.to, j.through)),
            Some((0x1016, true))
        );
    }

    #[test]
    fn the_entry_gets_a_relay_jump_padded_with_nops() {
        // Over a six byte `jmp qword [rip+x]`: the relay jump and one `nop`.
        let patch = entry_patch(0x1000, 0x2000, 6);
        let mut expected = vec![0xE9];
        expected.extend_from_slice(&(0x2000i32 - 0x1005).to_ne_bytes());
        expected.push(0x90);
        assert_eq!(patch.as_ref(), Ok(&expected));
        assert_eq!(
            patch.map(|p| leading_jump(&p, 0x1000)),
            Ok(Some(LeadingJump {
                to: 0x2000,
                through: false,
                len: 5,
            }))
        );
        // Over a five byte `jmp rel32`: exactly the relay jump.
        assert_eq!(entry_patch(0x1000, 0x2000, 5).map(|p| p.len()), Ok(5));
        // A relay beyond two gigabytes is refused rather than truncated.
        assert!(entry_patch(0x1000, 0x1_0000_1000, 5).is_err());
    }

    #[test]
    fn chaining_steals_only_the_jump_and_keeps_it_for_removal() {
        // jmp qword [rip+2], an unrelated two bytes, then the other program's pointer.
        let mut entry = vec![0xFF, 0x25, 0x02, 0x00, 0x00, 0x00, 0xCC, 0xCC];
        entry.extend_from_slice(&0x1234_5678usize.to_ne_bytes());
        entry.resize(32, 0xCC);
        let target = entry.as_ptr() as usize;
        let written = core::cell::RefCell::new(Vec::new());
        // SAFETY: `entry` is readable for its length; `write` only records, so the replacement
        // is never run.
        let detour = unsafe {
            install(
                target,
                entry.len(),
                0xDEAD_BEEF,
                |_| {},
                |at, bytes| {
                    assert_eq!(at, target);
                    bytes.clone_into(&mut written.borrow_mut());
                    Ok(entry[..bytes.len()].to_vec())
                },
            )
        };
        let Ok(detour) = detour else {
            panic!("{:?}", detour.err());
        };
        assert_eq!(detour.original, entry[..6]);
        let patch = written.borrow();
        assert_eq!(patch.len(), 6);
        assert_eq!((patch[0], patch[5]), (0xE9, 0x90));

        // SAFETY: the trampoline page is committed and holds at least its six byte jump.
        let code = unsafe { core::slice::from_raw_parts(detour.trampoline.cast::<u8>(), 6) };
        let jump = leading_jump(code, detour.trampoline as u64);
        assert_eq!(
            jump.map(|j| (j.to, j.through)),
            Some((target as u64 + 8, true))
        );
    }
}
