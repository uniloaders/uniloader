//! Inline function detours: the jump written at a function's entry, and the trampoline that
//! still reaches the original.
//!
//! The shape is the usual one. The target's first instructions are decoded until at least a
//! five byte `jmp rel32` fits, copied into a trampoline that ends in an absolute jump back to
//! the rest of the function, and the entry is replaced with a jump to the plugin. Because a
//! plugin DLL can sit further than two gigabytes from the game's code, the five byte jump goes
//! to a relay allocated *near the target*, and the relay performs the 14 byte absolute jump.
//!
//! What is deliberately refused rather than guessed:
//!
//! - an instruction in the stolen bytes that is relative to the instruction pointer, which
//!   would mean something different once it lives in the trampoline;
//! - a branch, call or return in the stolen bytes, for the same reason;
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
/// # Safety
/// `target` must be the entry point of a function and `replacement` must have its exact
/// signature and calling convention.
pub(super) unsafe fn install(
    target: usize,
    replacement: usize,
    write: impl Fn(usize, &[u8]) -> Result<Vec<u8>, Status>,
) -> Result<Detour, String> {
    // SAFETY: the caller established that `target` is committed code; reading the longest
    // possible stolen prologue from it is what the decoder needs.
    let entry = unsafe { core::slice::from_raw_parts(target as *const u8, MAX_INSTRUCTIONS * 16) };
    let stolen = stolen_length(entry, target as u64)?;

    let trampoline = alloc_executable(None).ok_or("cannot allocate a trampoline page")?;
    let relay = alloc_executable(Some(target)).ok_or_else(|| {
        format!("cannot allocate a relay page within {NEAR_RANGE:#X} of {target:#X}")
    })?;

    // SAFETY: both pages are freshly committed, writable and at least `stolen +
    // ABSOLUTE_JUMP_LEN` long, which is well under one page.
    unsafe {
        core::ptr::copy_nonoverlapping(target as *const u8, trampoline, stolen);
        let back = absolute_jump(target + stolen);
        core::ptr::copy_nonoverlapping(back.as_ptr(), trampoline.add(stolen), back.len());
        let to_plugin = absolute_jump(replacement);
        core::ptr::copy_nonoverlapping(to_plugin.as_ptr(), relay, to_plugin.len());
    }

    // `jmp rel32` to the relay, then `nop` over whatever is left of the stolen bytes so the
    // entry stays a sequence of whole instructions.
    // Through `i64` rather than `isize` arithmetic: a user-mode address never exceeds the
    // positive range, and the conversion says so instead of assuming it.
    let reach = |what: usize| i64::try_from(what).map_err(|_| "address out of range".to_owned());
    // A `jmp rel32` counts from the end of the instruction, hence `target + JUMP_LEN`.
    let displacement = reach(relay as usize)? - reach(target + JUMP_LEN)?;
    let displacement = i32::try_from(displacement)
        .map_err(|_| format!("the relay at {relay:p} is out of reach of {target:#X}"))?;
    let mut patch = vec![0x90u8; stolen];
    patch[0] = 0xE9;
    patch[1..JUMP_LEN].copy_from_slice(&displacement.to_ne_bytes());

    let original = write(target, &patch).map_err(|e| format!("cannot write the jump: {e:?}"))?;
    log::debug!(
        "detoured {target:#X} -> {replacement:#X} via relay {relay:p}, {stolen} bytes stolen"
    );
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

        // jmp rel32 as the very first instruction: a thunk, not a function body.
        let jump = [0xE9, 0x00, 0x10, 0x00, 0x00, 0x90];
        let refusal = stolen_length(&jump, 0x1000);
        assert!(
            matches!(&refusal, Err(e) if e.contains("changes control flow")),
            "{refusal:?}"
        );

        // ret immediately: nothing to steal.
        let ret = [0xC3, 0x90, 0x90, 0x90, 0x90, 0x90];
        assert!(stolen_length(&ret, 0x1000).is_err());
    }
}
