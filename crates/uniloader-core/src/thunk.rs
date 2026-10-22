//! Recognising an x86-64 function entry that is only a jump somewhere else.
//!
//! System DLL exports are often thunks: `kernel32!QueryPerformanceCounter` is a
//! `jmp qword [rip+x]` into `ntdll`, and a proxy DLL's forwarder is the same shape. A detour
//! cannot be put on a thunk (moving a relative jump changes where it goes), and would miss
//! every caller that reaches the real function another way, so a hook follows thunks to the
//! function body first.

/// Where a thunk's jump goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jump {
    /// Straight to this address.
    To(u64),
    /// To the address stored at this address.
    Through(u64),
}

/// The jump at the start of `code`, which is at `address`, if `code` starts with one.
///
/// Recognises `jmp rel32`, `jmp rel8` and `jmp qword [rip+disp32]`, with or without a REX.W
/// prefix: what compilers and linkers emit for thunks and import stubs.
#[must_use]
pub fn jump(code: &[u8], address: u64) -> Option<Jump> {
    let relative = |len: u64, displacement: i64| {
        address
            .checked_add(len)
            .and_then(|end| end.checked_add_signed(displacement))
    };
    match code {
        [0xE9, a, b, c, d, ..] => {
            relative(5, i64::from(i32::from_le_bytes([*a, *b, *c, *d]))).map(Jump::To)
        }
        [0xEB, rel, ..] => relative(2, i64::from(i8::from_le_bytes([*rel]))).map(Jump::To),
        [0xFF, 0x25, a, b, c, d, ..] => {
            relative(6, i64::from(i32::from_le_bytes([*a, *b, *c, *d]))).map(Jump::Through)
        }
        [0x48, 0xFF, 0x25, a, b, c, d, ..] => {
            relative(7, i64::from(i32::from_le_bytes([*a, *b, *c, *d]))).map(Jump::Through)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_jumps_go_forwards_and_backwards() {
        assert_eq!(jump(&[0xE9, 0x10, 0, 0, 0], 0x1000), Some(Jump::To(0x1015)));
        assert_eq!(
            jump(&[0xE9, 0xFB, 0xFF, 0xFF, 0xFF], 0x1000),
            Some(Jump::To(0x1000))
        );
        assert_eq!(jump(&[0xEB, 0xFE], 0x1000), Some(Jump::To(0x1000)));
    }

    #[test]
    fn indirect_jumps_name_the_slot() {
        assert_eq!(
            jump(&[0xFF, 0x25, 0x00, 0x01, 0, 0], 0x2000),
            Some(Jump::Through(0x2106))
        );
        assert_eq!(
            jump(&[0x48, 0xFF, 0x25, 0x00, 0x01, 0, 0], 0x2000),
            Some(Jump::Through(0x2107))
        );
    }

    #[test]
    fn a_function_body_is_not_a_jump() {
        // mov [rsp+8], rbx: the start of ntdll!RtlQueryPerformanceCounter.
        assert_eq!(jump(&[0x48, 0x89, 0x5C, 0x24, 0x08], 0x1000), None);
        // Too short to hold the displacement.
        assert_eq!(jump(&[0xE9, 0x10], 0x1000), None);
        assert_eq!(jump(&[], 0x1000), None);
    }
}
