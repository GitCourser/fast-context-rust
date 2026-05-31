use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;

use crate::error::{FastContextError, FastContextErrorKind};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProtobufEncoder {
    chunks: Vec<Vec<u8>>,
}

impl ProtobufEncoder {
    #[must_use]
    pub fn new() -> Self {
        Self { chunks: Vec::new() }
    }

    fn encode_varint(mut value: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        while value > 0x7f {
            bytes.push(((value & 0x7f) as u8) | 0x80);
            value >>= 7;
        }
        bytes.push((value & 0x7f) as u8);
        bytes
    }

    fn tag(field: u32, wire: u8) -> Vec<u8> {
        Self::encode_varint(((field as u64) << 3) | u64::from(wire))
    }

    pub fn write_varint(mut self, field: u32, value: u64) -> Self {
        self.chunks.push(Self::tag(field, 0));
        self.chunks.push(Self::encode_varint(value));
        self
    }

    pub fn write_string(self, field: u32, value: &str) -> Self {
        self.write_bytes(field, value.as_bytes())
    }

    pub fn write_bytes(mut self, field: u32, value: &[u8]) -> Self {
        self.chunks.push(Self::tag(field, 2));
        self.chunks.push(Self::encode_varint(value.len() as u64));
        self.chunks.push(value.to_vec());
        self
    }

    pub fn write_message(self, field: u32, sub: &ProtobufEncoder) -> Self {
        self.write_bytes(field, &sub.to_vec())
    }

    #[must_use]
    pub fn to_vec(&self) -> Vec<u8> {
        self.chunks.iter().flatten().copied().collect()
    }
}

pub fn decode_varint(data: &[u8], mut offset: usize) -> Result<(u64, usize), FastContextError> {
    let mut value = 0_u64;
    let mut shift = 0_u32;

    while offset < data.len() {
        let byte = data[offset];
        offset += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok((value, offset));
        }
        shift += 7;
        if shift >= 64 {
            return Err(FastContextError::new(
                FastContextErrorKind::InvalidResponse,
                "protobuf varint is too large",
            ));
        }
    }

    Err(FastContextError::new(
        FastContextErrorKind::InvalidResponse,
        "truncated protobuf varint",
    ))
}

#[must_use]
pub fn extract_strings(data: &[u8]) -> Vec<String> {
    let mut strings = Vec::new();
    let mut offset = 0_usize;

    while offset < data.len() {
        let Ok((tag, next)) = decode_varint(data, offset) else {
            break;
        };
        offset = next;
        let wire = tag & 0x07;

        match wire {
            0 => {
                let Ok((_, next)) = decode_varint(data, offset) else {
                    break;
                };
                offset = next;
            }
            1 => offset = offset.saturating_add(8),
            2 => {
                let Ok((length, next)) = decode_varint(data, offset) else {
                    break;
                };
                offset = next;
                let length = length as usize;
                let Some(end) = offset.checked_add(length) else {
                    break;
                };
                if end > data.len() {
                    break;
                }
                let raw = &data[offset..end];
                if let Ok(text) = std::str::from_utf8(raw)
                    && text.chars().count() > 5
                {
                    strings.push(text.to_string());
                }
                offset = end;
            }
            5 => offset = offset.saturating_add(4),
            _ => break,
        }

        if offset > data.len() {
            break;
        }
    }

    strings
}

pub fn connect_frame_encode(
    proto_bytes: &[u8],
    compress: bool,
) -> Result<Vec<u8>, FastContextError> {
    let (flags, payload) = if compress {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(proto_bytes).map_err(|error| {
            FastContextError::new(
                FastContextErrorKind::InvalidResponse,
                format!("gzip encode failed: {error}"),
            )
        })?;
        let payload = encoder.finish().map_err(|error| {
            FastContextError::new(
                FastContextErrorKind::InvalidResponse,
                format!("gzip finish failed: {error}"),
            )
        })?;
        (1_u8, payload)
    } else {
        (0_u8, proto_bytes.to_vec())
    };

    let length: u32 = payload.len().try_into().map_err(|_| {
        FastContextError::new(
            FastContextErrorKind::PayloadTooLarge,
            "connect frame payload exceeds u32 length",
        )
    })?;
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(flags);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

pub fn connect_frame_decode(data: &[u8]) -> Result<Vec<Vec<u8>>, FastContextError> {
    let mut frames = Vec::new();
    let mut offset = 0_usize;

    while offset + 5 <= data.len() {
        let flags = data[offset];
        let length = u32::from_be_bytes([
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
            data[offset + 4],
        ]) as usize;
        offset += 5;
        let Some(end) = offset.checked_add(length) else {
            return Err(FastContextError::new(
                FastContextErrorKind::InvalidResponse,
                "connect frame length overflow",
            ));
        };
        if end > data.len() {
            return Err(FastContextError::new(
                FastContextErrorKind::InvalidResponse,
                "truncated connect frame payload",
            ));
        }

        let payload = &data[offset..end];
        let decoded = if flags == 1 || flags == 3 {
            let mut decoder = GzDecoder::new(payload);
            let mut out = Vec::new();
            match decoder.read_to_end(&mut out) {
                Ok(_) => out,
                Err(_) => payload.to_vec(),
            }
        } else {
            payload.to_vec()
        };
        frames.push(decoded);
        offset = end;
    }

    if offset != data.len() {
        return Err(FastContextError::new(
            FastContextErrorKind::InvalidResponse,
            "connect frame has trailing partial header bytes",
        ));
    }

    Ok(frames)
}
