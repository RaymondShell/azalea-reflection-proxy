//! Frame-level adapters over azalea's raw connection halves. The pumps
//! themselves live in session.rs (phase 2 made them session-aware); this
//! module just turns raw packet bytes into Frames and back.

use std::io::Cursor;

use azalea_protocol::connect::{RawReadConnection, RawWriteConnection};
use azalea_buf::AzBufVar;
use eyre::Result;

use crate::plugin::Frame;

/// Abstract over "somewhere frames come from / go to" so the pumps don't
/// care which leg they're attached to.
#[async_trait::async_trait]
pub trait FrameSource: Send {
    async fn read_frame(&mut self) -> Result<Frame>;
}
#[async_trait::async_trait]
pub trait FrameSink: Send {
    async fn write_frame(&mut self, frame: Frame) -> Result<()>;
}

/// Implementation for azalea's RawReadConnection
pub struct AzaleaFrameSource {
    pub reader: RawReadConnection,
}

#[async_trait::async_trait]
impl FrameSource for AzaleaFrameSource {
    async fn read_frame(&mut self) -> Result<Frame> {
        // Read raw packet bytes (includes packet ID + body)
        let raw_packet = self.reader.read().await
            .map_err(|e| eyre::eyre!("Failed to read packet: {:?}", e))?;

        // Extract packet ID from the beginning
        let mut cursor = Cursor::new(&*raw_packet);
        let packet_id = u32::azalea_read_var(&mut cursor)
            .map_err(|e| eyre::eyre!("Failed to read packet ID: {:?}", e))?;

        // Rest is the body
        let body_start = cursor.position() as usize;
        let body = raw_packet[body_start..].to_vec();

        Ok(Frame {
            packet_id,
            body,
        })
    }
}

/// Implementation for azalea's RawWriteConnection
pub struct AzaleaFrameSink {
    pub writer: RawWriteConnection,
}

#[async_trait::async_trait]
impl FrameSink for AzaleaFrameSink {
    async fn write_frame(&mut self, frame: Frame) -> Result<()> {
        // Encode packet ID + body
        let mut raw_packet = Vec::new();
        frame.packet_id.azalea_write_var(&mut raw_packet)
            .map_err(|e| eyre::eyre!("Failed to write packet ID: {:?}", e))?;
        raw_packet.extend_from_slice(&frame.body);

        // Write raw packet
        self.writer.write(&raw_packet).await
            .map_err(|e| eyre::eyre!("Failed to write packet: {:?}", e))?;

        Ok(())
    }
}

