//! framing. every transport (unix socket now, quic/websocket later) carries
//! the same frames:
//!
//! ```text
//! u32 big-endian payload length | u8 kind | payload
//! ```
//!
//! kind 0 is a json `Envelope`. kind 1 is a raw binary chunk, reserved for
//! terminal bytes. unknown kinds are delivered as-is so a reader can skip them.

pub const KIND_JSON: u8 = 0;
pub const KIND_BINARY: u8 = 1;

/// refuse anything bigger. a corrupt length must not make us allocate gigabytes.
pub const MAX_PAYLOAD: usize = 32 * 1024 * 1024;

const HEADER: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub kind: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("frame payload of {0} bytes exceeds the {MAX_PAYLOAD} byte limit")]
    TooLarge(usize),
}

pub fn encode(kind: u8, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(FrameError::TooLarge(payload.len()));
    }
    let mut out = Vec::with_capacity(HEADER + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.push(kind);
    out.extend_from_slice(payload);
    Ok(out)
}

/// incremental decoder: feed it whatever bytes arrived, take frames out.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
    /// read position in `buf`; consumed bytes are dropped lazily.
    pos: usize,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if self.pos > 0 && self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        } else if self.pos > 64 * 1024 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// the next complete frame, or `None` if more bytes are needed.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        let avail = &self.buf[self.pos..];
        if avail.len() < HEADER {
            return Ok(None);
        }
        let len = u32::from_be_bytes([avail[0], avail[1], avail[2], avail[3]]) as usize;
        if len > MAX_PAYLOAD {
            return Err(FrameError::TooLarge(len));
        }
        if avail.len() < HEADER + len {
            return Ok(None);
        }
        let frame = Frame { kind: avail[4], payload: avail[HEADER..HEADER + len].to_vec() };
        self.pos += HEADER + len;
        Ok(Some(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let bytes = encode(KIND_JSON, b"{\"a\":1}").unwrap();
        let mut d = Decoder::new();
        d.push(&bytes);
        assert_eq!(d.next_frame().unwrap(), Some(Frame { kind: KIND_JSON, payload: b"{\"a\":1}".to_vec() }));
        assert_eq!(d.next_frame().unwrap(), None);
    }

    #[test]
    fn frames_split_at_every_byte_boundary() {
        let mut stream = Vec::new();
        stream.extend(encode(KIND_JSON, b"first").unwrap());
        stream.extend(encode(KIND_BINARY, &[0, 1, 2, 255]).unwrap());
        stream.extend(encode(KIND_JSON, b"").unwrap());
        for split in 0..=stream.len() {
            let mut d = Decoder::new();
            let mut got = Vec::new();
            for chunk in [&stream[..split], &stream[split..]] {
                d.push(chunk);
                while let Some(f) = d.next_frame().unwrap() {
                    got.push(f);
                }
            }
            assert_eq!(got.len(), 3, "split at {split}");
            assert_eq!(got[0].payload, b"first");
            assert_eq!((got[1].kind, got[1].payload.as_slice()), (KIND_BINARY, &[0u8, 1, 2, 255][..]));
            assert!(got[2].payload.is_empty());
        }
    }

    #[test]
    fn one_byte_at_a_time() {
        let stream = encode(KIND_JSON, b"hello").unwrap();
        let mut d = Decoder::new();
        let mut got = None;
        for b in &stream {
            d.push(std::slice::from_ref(b));
            if let Some(f) = d.next_frame().unwrap() {
                got = Some(f);
            }
        }
        assert_eq!(got.unwrap().payload, b"hello");
    }

    #[test]
    fn oversized_length_is_an_error_not_an_allocation() {
        let mut d = Decoder::new();
        d.push(&[0xff, 0xff, 0xff, 0xff, 0]);
        assert_eq!(d.next_frame(), Err(FrameError::TooLarge(u32::MAX as usize)));
        assert!(encode(KIND_JSON, &vec![0; MAX_PAYLOAD + 1]).is_err());
    }

    #[test]
    fn unknown_kind_is_passed_through() {
        let mut d = Decoder::new();
        d.push(&encode(42, b"x").unwrap());
        assert_eq!(d.next_frame().unwrap().unwrap().kind, 42);
    }
}
