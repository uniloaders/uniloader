//! Version numbers of DLLs the loader did not build: `.asi` files and chain-loaded libraries
//! such as UE4SS, so they can be listed next to the plugins with a version.
//!
//! Read from the file's bytes rather than through `GetFileVersionInfoW`: the loader may itself
//! be standing in for `version.dll`, and a byte scan works the same on every host, which keeps
//! it testable here.

/// `VS_FIXEDFILEINFO::dwSignature`, little endian.
const SIGNATURE: [u8; 4] = 0xFEEF_04BD_u32.to_le_bytes();

/// Bytes of `VS_FIXEDFILEINFO` up to and including the product version.
const FIXED_LEN: usize = 6 * 4;

fn word(bytes: &[u8], at: usize) -> Option<u32> {
    let slice = bytes.get(at..at + 4)?;
    Some(u32::from_le_bytes(slice.try_into().ok()?))
}

/// The product version from the file's `VS_FIXEDFILEINFO`, as `major.minor.patch.build`, or
/// `None` when the file has no version resource.
///
/// The product version rather than the file version: it is the one projects bump per release,
/// while some leave the file version at `1.0.0.0`. A trailing `.0` build number is dropped,
/// since `3.0.1` is how such a release is named everywhere else.
#[must_use]
pub fn product_version(bytes: &[u8]) -> Option<String> {
    // The structure is DWORD aligned inside the resource, and the resource section is aligned
    // too, so only aligned positions need checking.
    let start = (0..bytes.len().saturating_sub(FIXED_LEN))
        .step_by(4)
        .find(|&i| bytes[i..i + 4] == SIGNATURE)?;
    let ms = word(bytes, start + 16)?;
    let ls = word(bytes, start + 20)?;
    let parts = [ms >> 16, ms & 0xFFFF, ls >> 16, ls & 0xFFFF];
    let shown = if parts[3] == 0 {
        &parts[..3]
    } else {
        &parts[..]
    };
    Some(
        shown
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join("."),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(high: u32, low: u32, offset: usize) -> Vec<u8> {
        let mut bytes = vec![0u8; offset];
        for value in [0xFEEF_04BD, 0x0001_0000, 0x0001_0000, 0, high, low] {
            bytes.extend_from_slice(&u32::to_le_bytes(value));
        }
        bytes.extend_from_slice(&[0u8; 32]);
        bytes
    }

    #[test]
    fn reads_the_product_version() {
        assert_eq!(
            product_version(&fixed(0x0003_0000, 0x0001_0000, 64)).as_deref(),
            Some("3.0.1")
        );
        assert_eq!(
            product_version(&fixed(0x0002_0005, 0x0000_0007, 128)).as_deref(),
            Some("2.5.0.7")
        );
    }

    #[test]
    fn no_resource_and_misaligned_signatures_are_none() {
        assert_eq!(product_version(&[0u8; 256]), None);
        assert_eq!(product_version(&fixed(0x0003_0000, 0, 66)), None);
        assert_eq!(product_version(&[]), None);
    }
}
