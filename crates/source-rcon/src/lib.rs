//! The Source RCON wire format, as documented on the Valve developer wiki ("Source RCON
//! Protocol").
//!
//! A packet is `i32 size` (little-endian, the number of bytes after this field), `i32 id`,
//! `i32 type`, the body as NUL-terminated text, then one more NUL. This crate encodes and
//! decodes packets, caps their size on the way in, and splits long output into several
//! responses. Connections, authentication and limits are the server's business.
//!
//! Written for uniloader because the RCON crates on crates.io are clients, most of them tied
//! to an async runtime; a server needs only this much of the format.

use std::io::{self, Read, Write};

use thiserror::Error;

/// Client to server: authenticate with the password in the body.
pub const AUTH: i32 = 3;
/// Server to client: the answer to [`AUTH`]; the id is the request's, or -1 on failure.
pub const AUTH_RESPONSE: i32 = 2;
/// Client to server: run the body as a command.
pub const EXEC_COMMAND: i32 = 2;
/// Server to client: command output. Also sent by clients, empty, as an end marker the
/// server mirrors back.
pub const RESPONSE_VALUE: i32 = 0;

/// The id a failed authentication is answered with.
pub const AUTH_FAILED_ID: i32 = -1;

/// Longest body in one response; longer output is split over several.
pub const MAX_RESPONSE_BODY: usize = 4096;

/// Bytes of a packet besides its body: id, type and the two NULs.
const OVERHEAD: usize = 10;

/// One packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// Chosen by the client; responses carry the id of their request.
    pub id: i32,
    /// [`AUTH`], [`EXEC_COMMAND`]/[`AUTH_RESPONSE`] or [`RESPONSE_VALUE`].
    pub kind: i32,
    /// The text, without its terminating NUL.
    pub body: String,
}

/// Why a packet could not be read.
#[derive(Debug, Error)]
pub enum DecodeError {
    /// The connection failed or timed out.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// The size field is smaller than an empty packet or larger than the cap.
    #[error("packet size {0} is out of range")]
    Size(i32),
}

impl Packet {
    /// A packet with `body`.
    #[must_use]
    pub fn new(id: i32, kind: i32, body: &str) -> Self {
        Packet {
            id,
            kind,
            body: body.to_owned(),
        }
    }

    /// The packet in wire form. A NUL inside the body would end it early on the other side,
    /// so the body is cut there.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let body = self.body.split('\0').next().unwrap_or_default().as_bytes();
        let size = i32::try_from(body.len() + OVERHEAD).unwrap_or(i32::MAX);
        let mut out = Vec::with_capacity(body.len() + OVERHEAD + 4);
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&self.id.to_le_bytes());
        out.extend_from_slice(&self.kind.to_le_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(&[0, 0]);
        out
    }

    /// Write the packet to `w`.
    ///
    /// # Errors
    /// The write failed.
    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(&self.encode())
    }

    /// Read one packet whose body is at most `max_body` bytes. Body bytes after the first
    /// NUL are ignored, and text that is not UTF-8 is read lossily.
    ///
    /// # Errors
    /// The read failed, or the size field is out of range; the caller should then close the
    /// connection, because the stream is no longer in step.
    pub fn read_from(r: &mut impl Read, max_body: usize) -> Result<Self, DecodeError> {
        let mut word = [0u8; 4];
        r.read_exact(&mut word)?;
        let size = i32::from_le_bytes(word);
        let len = usize::try_from(size).map_err(|_| DecodeError::Size(size))?;
        if !(OVERHEAD..=max_body + OVERHEAD).contains(&len) {
            return Err(DecodeError::Size(size));
        }
        let mut rest = vec![0u8; len];
        r.read_exact(&mut rest)?;
        let field = |at: usize| {
            let mut bytes = [0u8; 4];
            bytes.copy_from_slice(&rest[at..at + 4]);
            i32::from_le_bytes(bytes)
        };
        let (id, kind) = (field(0), field(4));
        let body = &rest[8..];
        let end = body.iter().position(|b| *b == 0).unwrap_or(body.len());
        Ok(Packet {
            id,
            kind,
            body: String::from_utf8_lossy(&body[..end]).into_owned(),
        })
    }
}

/// `text` as [`RESPONSE_VALUE`] packets with `id`, each body at most [`MAX_RESPONSE_BODY`]
/// bytes and cut at character boundaries. Empty text is one empty packet.
#[must_use]
pub fn responses(id: i32, text: &str) -> Vec<Packet> {
    let mut out = Vec::new();
    let mut rest = text;
    while rest.len() > MAX_RESPONSE_BODY {
        let mut cut = MAX_RESPONSE_BODY;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        out.push(Packet::new(id, RESPONSE_VALUE, &rest[..cut]));
        rest = &rest[cut..];
    }
    out.push(Packet::new(id, RESPONSE_VALUE, rest));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_the_wiki_example() {
        // The wiki's auth example: id 0, type 3, body "passwrd".
        let bytes = Packet::new(0, AUTH, "passwrd").encode();
        let expected: Vec<u8> = [
            &[0x11, 0, 0, 0][..],
            &[0, 0, 0, 0],
            &[3, 0, 0, 0],
            b"passwrd",
            &[0, 0],
        ]
        .concat();
        assert_eq!(bytes, expected);
        let back = Packet::read_from(&mut bytes.as_slice(), 4096).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back, Packet::new(0, AUTH, "passwrd"));
    }

    #[test]
    fn rejects_sizes_out_of_range() {
        let mut big = Packet::new(1, EXEC_COMMAND, &"x".repeat(100)).encode();
        assert!(matches!(
            Packet::read_from(&mut big.as_slice(), 50),
            Err(DecodeError::Size(110))
        ));
        big[..4].copy_from_slice(&(-5i32).to_le_bytes());
        assert!(matches!(
            Packet::read_from(&mut big.as_slice(), 4096),
            Err(DecodeError::Size(-5))
        ));
        assert!(matches!(
            Packet::read_from(&mut [1u8, 0].as_slice(), 4096),
            Err(DecodeError::Io(_))
        ));
    }

    #[test]
    fn long_output_splits_on_character_boundaries() {
        let text = "ä".repeat(3000); // 6000 bytes
        let packets = responses(7, &text);
        assert_eq!(packets.len(), 2);
        assert!(packets
            .iter()
            .all(|p| p.id == 7 && p.body.len() <= MAX_RESPONSE_BODY));
        assert_eq!(packets.concat_bodies(), text);
        assert_eq!(responses(1, ""), vec![Packet::new(1, RESPONSE_VALUE, "")]);
    }

    trait Bodies {
        fn concat_bodies(&self) -> String;
    }

    impl Bodies for Vec<Packet> {
        fn concat_bodies(&self) -> String {
            self.iter().map(|p| p.body.as_str()).collect()
        }
    }

    #[test]
    fn a_nul_in_the_body_ends_it() {
        let bytes = Packet::new(2, EXEC_COMMAND, "echo a\0b").encode();
        let back = Packet::read_from(&mut bytes.as_slice(), 4096).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back.body, "echo a");
    }
}
