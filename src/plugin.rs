//! Plugin pipeline — the azalea equivalent of the JS project's plugin
//! system (anonymize / snapshot / synchronization / inventory / replicator).
//!
//! Phase 1 runs zero plugins and just forwards everything; the trait exists
//! now so phases 2-4 are additive instead of a rewrite. Hooks mirror the
//! original's: onReadReal ≈ on_clientbound (packets from the target
//! server), onWriteReal ≈ on_serverbound (packets from the controlling
//! client), bindToReflected ≈ on_session_start.
//!
//! Hooks operate on RAW FRAMES (packet id + payload bytes, post
//! decryption/decompression) rather than typed packets. This is deliberate:
//! phase 1 doesn't need to understand packets, and raw frames survive
//! protocol details the proxy doesn't model. Plugins that need typed access
//! (snapshot, replicator) parse the frames they care about themselves via
//! azalea_protocol's read functions and ignore the rest.

/// A raw protocol frame: varint packet id + body, already stripped of
/// length prefix / compression / encryption.
#[derive(Clone, Debug)]
pub struct Frame {
    pub packet_id: u32,
    pub body: Vec<u8>,
}

/// What the pipeline should do with a frame after a plugin saw it.
pub enum Verdict {
    /// pass it along unchanged (the overwhelmingly common case)
    Forward,
    /// swallow it (e.g. replicator answering a viewer's keepalive locally)
    Drop,
    /// substitute different frame(s) (e.g. anonymize rewriting names)
    Replace(Vec<Frame>),
}

pub trait ProxyPlugin: Send + Sync {
    fn name(&self) -> &'static str;

    /// A new session (upstream connection) has been established.
    fn on_session_start(&self) {}

    /// Frame travelling target-server -> clients.
    fn on_clientbound(&self, _frame: &Frame) -> Verdict {
        Verdict::Forward
    }

    /// Frame travelling controlling-client -> target server.
    fn on_serverbound(&self, _frame: &Frame) -> Verdict {
        Verdict::Forward
    }
}

/// Runs frames through every plugin in order. First Drop/Replace wins,
/// matching the original's sequential plugin order semantics.
pub struct Pipeline {
    pub plugins: Vec<Box<dyn ProxyPlugin>>,
}

impl Pipeline {
    pub fn clientbound(&self, frame: Frame) -> Vec<Frame> {
        self.route(frame, true)
    }
    pub fn serverbound(&self, frame: Frame) -> Vec<Frame> {
        self.route(frame, false)
    }
    fn route(&self, frame: Frame, clientbound: bool) -> Vec<Frame> {
        for p in &self.plugins {
            let verdict = if clientbound {
                p.on_clientbound(&frame)
            } else {
                p.on_serverbound(&frame)
            };
            match verdict {
                Verdict::Forward => continue,
                Verdict::Drop => return Vec::new(),
                Verdict::Replace(frames) => return frames,
            }
        }
        vec![frame]
    }
}
