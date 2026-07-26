//! Frame-level adapters over azalea's raw connection halves. The pumps
//! themselves live in session.rs (phase 2 made them session-aware); this
//! module just turns raw packet bytes into Frames and back.

use std::io::Cursor;

use azalea_buf::AzBufVar;
use azalea_protocol::{
    connect::{RawReadConnection, RawWriteConnection},
    read::read_raw_packet_from_buffer,
};
use eyre::Result;
use tokio::io::AsyncReadExt;

use crate::plugin::Frame;

/// Matches Azalea's decompressed packet ceiling. The local leg is
/// intentionally uncompressed, and Azalea's raw reader otherwise keeps
/// buffering until a peer-supplied declared length is satisfied.
const MAX_LOCAL_FRAME_BYTES: usize = 8_388_608;

/// Implementation for azalea's RawReadConnection
pub struct AzaleaFrameSource {
    pub reader: RawReadConnection,
}

impl AzaleaFrameSource {
    pub async fn read_frame(&mut self) -> Result<Frame> {
        // Read raw packet bytes (includes packet ID + body)
        let raw_packet = self
            .reader
            .read()
            .await
            .map_err(|e| eyre::eyre!("Failed to read packet: {:?}", e))?;
        decode_frame(raw_packet)
    }

    /// Read an uncompressed, unencrypted local frame with a declared-length
    /// limit applied before the body is buffered.
    pub async fn read_local_frame(&mut self) -> Result<Frame> {
        let raw_packet = read_local_raw_packet(&mut self.reader).await?;
        decode_frame(raw_packet)
    }
}

fn decode_frame(raw_packet: Box<[u8]>) -> Result<Frame> {
    // Box<[u8]> -> Vec<u8> -> Bytes reuses the packet allocation. The body
    // is then a zero-copy slice instead of a second allocation per frame.
    let raw_packet = bytes::Bytes::from(Vec::from(raw_packet));
    let mut cursor = Cursor::new(raw_packet.as_ref());
    let packet_id = u32::azalea_read_var(&mut cursor)
        .map_err(|e| eyre::eyre!("Failed to read packet ID: {:?}", e))?;
    let body_start = cursor.position() as usize;
    let body = raw_packet.slice(body_start..);

    Ok(Frame { packet_id, body })
}

/// Read one packet from the local leg without allowing its frame buffer to
/// grow past the protocol ceiling. This is also used for typed handshake and
/// login reads before the connection becomes a raw frame source.
pub async fn read_local_raw_packet(reader: &mut RawReadConnection) -> Result<Box<[u8]>> {
    if reader.compression_threshold.is_some() || reader.dec_cipher.is_some() {
        eyre::bail!("bounded local reader requires an uncompressed, unencrypted connection");
    }

    loop {
        let position = reader.buffer.position() as usize;
        let pending = &reader.buffer.get_ref()[position..];
        if let Some(length) = declared_frame_length(pending)? {
            if length > MAX_LOCAL_FRAME_BYTES {
                eyre::bail!(
                    "local packet declares {length} bytes; maximum is {MAX_LOCAL_FRAME_BYTES}"
                );
            }
        }

        if let Some(packet) =
            read_raw_packet_from_buffer::<tokio::net::tcp::OwnedReadHalf>(&mut reader.buffer, None)?
        {
            if packet.len() > MAX_LOCAL_FRAME_BYTES {
                eyre::bail!(
                    "local packet contains {} bytes; maximum is {MAX_LOCAL_FRAME_BYTES}",
                    packet.len()
                );
            }
            return Ok(packet);
        }

        // Drop bytes belonging to already-consumed frames before appending
        // another partial frame. Without compaction, a continuous stream can
        // retain an arbitrarily large consumed prefix.
        let consumed = reader.buffer.position() as usize;
        if consumed > 0 {
            reader.buffer.get_mut().drain(..consumed);
            reader.buffer.set_position(0);
        }

        let mut chunk = [0u8; 16 * 1024];
        let read = reader.read_stream.read(&mut chunk).await?;
        if read == 0 {
            eyre::bail!("local connection closed");
        }
        reader.buffer.get_mut().extend_from_slice(&chunk[..read]);
    }
}

fn declared_frame_length(bytes: &[u8]) -> Result<Option<usize>> {
    let mut value = 0u32;
    for (index, &byte) in bytes.iter().take(5).enumerate() {
        value |= u32::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return Ok(Some(value as usize));
        }
    }
    if bytes.len() >= 5 {
        eyre::bail!("local packet has an invalid length VarInt");
    }
    Ok(None)
}

/// Implementation for azalea's RawWriteConnection
pub struct AzaleaFrameSink {
    pub writer: RawWriteConnection,
}

impl AzaleaFrameSink {
    pub async fn write_frame(&mut self, frame: Frame) -> Result<()> {
        // Encode packet ID + body
        let mut raw_packet = Vec::with_capacity(frame.body.len() + 5);
        frame
            .packet_id
            .azalea_write_var(&mut raw_packet)
            .map_err(|e| eyre::eyre!("Failed to write packet ID: {:?}", e))?;
        raw_packet.extend_from_slice(&frame.body);

        // Write raw packet
        self.writer
            .write(&raw_packet)
            .await
            .map_err(|e| eyre::eyre!("Failed to write packet: {:?}", e))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_frame_length_rejects_overlong_varints() {
        assert_eq!(declared_frame_length(&[0x7f]).unwrap(), Some(127));
        assert_eq!(declared_frame_length(&[0x80, 0x40]).unwrap(), Some(8_192));
        assert!(declared_frame_length(&[0x80; 5]).is_err());
    }
}
