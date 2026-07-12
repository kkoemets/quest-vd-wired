use std::{fmt, str::FromStr};

use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAGIC: [u8; 4] = *b"GNR4";
pub const VERSION: u16 = 4;
pub const HEADER_LEN: usize = 28;
pub const MAX_PAYLOAD_LEN: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct SessionId(pub [u8; 16]);

impl SessionId {
    pub const ZERO: Self = Self([0; 16]);

    pub fn random() -> Self {
        loop {
            let mut bytes = [0; 16];
            OsRng.fill_bytes(&mut bytes);
            let session = Self(bytes);
            if session != Self::ZERO {
                return session;
            }
        }
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.into_iter().enumerate() {
            if matches!(index, 4 | 6 | 8 | 10) {
                formatter.write_str("-")?;
            }
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for SessionId {
    type Err = ProtocolError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 36 {
            return Err(ProtocolError::InvalidSessionId);
        }
        let encoded = value.as_bytes();
        if !encoded.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        }) {
            return Err(ProtocolError::InvalidSessionId);
        }

        let mut hex = encoded
            .iter()
            .copied()
            .filter(|byte| *byte != b'-')
            .map(hex_nibble);
        let mut bytes = [0; 16];
        for destination in &mut bytes {
            let high = hex.next().ok_or(ProtocolError::InvalidSessionId)??;
            let low = hex.next().ok_or(ProtocolError::InvalidSessionId)??;
            *destination = (high << 4) | low;
        }
        let session = Self(bytes);
        if session == Self::ZERO {
            Err(ProtocolError::InvalidSessionId)
        } else {
            Ok(session)
        }
    }
}

fn hex_nibble(byte: u8) -> Result<u8, ProtocolError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(ProtocolError::InvalidSessionId),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[repr(u16)]
pub enum MessageType {
    Hello = 1,
    HelloAck = 2,
    Started = 3,
    Heartbeat = 4,
    Stop = 5,
    Stopped = 6,
    Status = 7,
    Error = 8,
}

impl TryFrom<u16> for MessageType {
    type Error = ProtocolError;

    fn try_from(value: u16) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(Self::Hello),
            2 => Ok(Self::HelloAck),
            3 => Ok(Self::Started),
            4 => Ok(Self::Heartbeat),
            5 => Ok(Self::Stop),
            6 => Ok(Self::Stopped),
            7 => Ok(Self::Status),
            8 => Ok(Self::Error),
            other => Err(ProtocolError::UnknownMessageType(other)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    pub message_type: MessageType,
    pub session_id: SessionId,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(message_type: MessageType, session_id: SessionId, payload: Vec<u8>) -> Self {
        Self {
            message_type,
            session_id,
            payload,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        if self.payload.len() > MAX_PAYLOAD_LEN {
            return Err(ProtocolError::PayloadTooLarge(self.payload.len()));
        }
        let mut output = Vec::with_capacity(HEADER_LEN + self.payload.len());
        output.extend_from_slice(&MAGIC);
        output.extend_from_slice(&VERSION.to_be_bytes());
        output.extend_from_slice(&(self.message_type as u16).to_be_bytes());
        output.extend_from_slice(&(self.payload.len() as u32).to_be_bytes());
        output.extend_from_slice(&self.session_id.0);
        output.extend_from_slice(&self.payload);
        Ok(output)
    }

    /// Pure, allocation-bounded parser suitable for property/fuzz testing.
    pub fn decode(input: &[u8]) -> Result<Self, ProtocolError> {
        if input.len() < HEADER_LEN {
            return Err(ProtocolError::TruncatedHeader(input.len()));
        }
        if input[..4] != MAGIC {
            return Err(ProtocolError::BadMagic);
        }
        let version = u16::from_be_bytes([input[4], input[5]]);
        if version != VERSION {
            return Err(ProtocolError::UnsupportedVersion(version));
        }
        let message_type = MessageType::try_from(u16::from_be_bytes([input[6], input[7]]))?;
        let payload_len = u32::from_be_bytes([input[8], input[9], input[10], input[11]]) as usize;
        if payload_len > MAX_PAYLOAD_LEN {
            return Err(ProtocolError::PayloadTooLarge(payload_len));
        }
        let expected = HEADER_LEN + payload_len;
        if input.len() < expected {
            return Err(ProtocolError::TruncatedPayload {
                expected: payload_len,
                actual: input.len().saturating_sub(HEADER_LEN),
            });
        }
        if input.len() != expected {
            return Err(ProtocolError::TrailingBytes(input.len() - expected));
        }
        let mut session = [0; 16];
        session.copy_from_slice(&input[12..28]);
        Ok(Self {
            message_type,
            session_id: SessionId(session),
            payload: input[HEADER_LEN..expected].to_vec(),
        })
    }

    pub async fn read_from<R>(reader: &mut R) -> Result<Self, ProtocolError>
    where
        R: AsyncRead + Unpin,
    {
        let mut header = [0; HEADER_LEN];
        reader.read_exact(&mut header).await?;
        if header[..4] != MAGIC {
            return Err(ProtocolError::BadMagic);
        }
        let version = u16::from_be_bytes([header[4], header[5]]);
        if version != VERSION {
            return Err(ProtocolError::UnsupportedVersion(version));
        }
        let payload_len =
            u32::from_be_bytes([header[8], header[9], header[10], header[11]]) as usize;
        if payload_len > MAX_PAYLOAD_LEN {
            return Err(ProtocolError::PayloadTooLarge(payload_len));
        }
        let mut encoded = Vec::with_capacity(HEADER_LEN + payload_len);
        encoded.extend_from_slice(&header);
        encoded.resize(HEADER_LEN + payload_len, 0);
        reader.read_exact(&mut encoded[HEADER_LEN..]).await?;
        Self::decode(&encoded)
    }

    pub async fn write_to<W>(&self, writer: &mut W) -> Result<(), ProtocolError>
    where
        W: AsyncWrite + Unpin,
    {
        writer.write_all(&self.encode()?).await?;
        writer.flush().await?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("bad GNR4 magic")]
    BadMagic,
    #[error("invalid session id")]
    InvalidSessionId,
    #[error("payload length {0} exceeds the protocol bound")]
    PayloadTooLarge(usize),
    #[error("header is truncated at {0} bytes")]
    TruncatedHeader(usize),
    #[error("payload is truncated: expected {expected}, got {actual}")]
    TruncatedPayload { expected: usize, actual: usize },
    #[error("frame contains {0} trailing bytes")]
    TrailingBytes(usize),
    #[error("unknown GNR4 message type {0}")]
    UnknownMessageType(u16),
    #[error("unsupported GNR4 version {0}")]
    UnsupportedVersion(u16),
    #[error("control I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn round_trip() {
        let frame = Frame::new(
            MessageType::Heartbeat,
            SessionId([0x5a; 16]),
            b"bounded".to_vec(),
        );
        assert_eq!(Frame::decode(&frame.encode().unwrap()).unwrap(), frame);
    }

    #[test]
    fn session_id_matches_java_uuid_text_and_wire_order() {
        let session: SessionId = "00112233-4455-6677-8899-aabbccddeeff".parse().unwrap();
        assert_eq!(
            session.0,
            [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff,
            ]
        );
        assert_eq!(session.to_string(), "00112233-4455-6677-8899-aabbccddeeff");
    }

    #[test]
    fn session_id_requires_canonical_uuid_text() {
        for invalid in [
            "00112233445566778899aabbccddeeff",
            "00112233-44556677-8899-aabbccddeeff",
            "00112233-4455-6677-8899-aabbccddeefg",
            "00112233-4455-6677-8899-aabbccddeeé",
            "00000000-0000-0000-0000-000000000000",
        ] {
            assert!(matches!(
                invalid.parse::<SessionId>(),
                Err(ProtocolError::InvalidSessionId)
            ));
        }

        let upper: SessionId = "00112233-4455-6677-8899-AABBCCDDEEFF".parse().unwrap();
        assert_eq!(upper.to_string(), "00112233-4455-6677-8899-aabbccddeeff");
    }

    #[test]
    fn consumes_shared_kotlin_rust_status_fixture() {
        let hex = include_str!("../../../../protocol/fixtures/gnr4-status-v4.hex").trim();
        assert_eq!(hex.len() % 2, 0);
        let encoded: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
            .collect();
        let frame = Frame::decode(&encoded).unwrap();
        assert_eq!(frame.message_type, MessageType::Status);
        assert_eq!(
            frame.session_id,
            "00112233-4455-6677-8899-aabbccddeeff".parse().unwrap()
        );
        assert_eq!(frame.payload, [1, 2, 3]);
        assert_eq!(frame.encode().unwrap(), encoded);
    }

    #[test]
    fn rejects_declared_oversize_before_allocating() {
        let mut header = [0; HEADER_LEN];
        header[..4].copy_from_slice(&MAGIC);
        header[4..6].copy_from_slice(&VERSION.to_be_bytes());
        header[6..8].copy_from_slice(&(MessageType::Hello as u16).to_be_bytes());
        header[8..12].copy_from_slice(&((MAX_PAYLOAD_LEN + 1) as u32).to_be_bytes());
        assert!(matches!(
            Frame::decode(&header),
            Err(ProtocolError::PayloadTooLarge(_))
        ));
    }

    proptest! {
        #[test]
        fn arbitrary_input_never_panics(input in prop::collection::vec(any::<u8>(), 0..100_000)) {
            let _ = Frame::decode(&input);
        }

        #[test]
        fn payloads_round_trip(payload in prop::collection::vec(any::<u8>(), 0..4096)) {
            let frame = Frame::new(MessageType::Status, SessionId([7; 16]), payload);
            prop_assert_eq!(Frame::decode(&frame.encode().unwrap()).unwrap(), frame);
        }
    }
}
