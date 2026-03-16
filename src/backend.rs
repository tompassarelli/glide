use crate::algorithm::GlideState;
use anyhow::{Context, Result};
use log::{debug, info, warn};
use std::io::Write;
use std::net::TcpStream;

/// A backend receives glide state transitions and translates them
/// into whatever the consumer expects.
pub trait Backend {
    fn on_state_change(&mut self, state: GlideState);
}

/// Kanata backend: translates activation state into FakeKey press/release
/// over kanata's TCP protocol.
pub struct KanataClient {
    address: String,
    virtual_key: String,
    stream: Option<TcpStream>,
}

impl KanataClient {
    pub fn new(address: String, virtual_key: String) -> Self {
        Self {
            address,
            virtual_key,
            stream: None,
        }
    }

    fn ensure_connected(&mut self) -> Result<&mut TcpStream> {
        if self.stream.is_none() {
            info!("connecting to kanata at {}", self.address);
            let stream = TcpStream::connect(&self.address)
                .with_context(|| format!("failed to connect to kanata at {}", self.address))?;
            stream.set_nodelay(true)?;
            self.stream = Some(stream);
            info!("connected to kanata");
        }
        Ok(self.stream.as_mut().unwrap())
    }
}

impl Backend for KanataClient {
    fn on_state_change(&mut self, state: GlideState) {
        let action = match state {
            GlideState::Active => "Press",
            GlideState::Inactive => "Release",
        };

        let msg = format!(
            r#"{{"ActOnFakeKey":{{"name":"{}","action":"{}"}}}}"#,
            self.virtual_key, action
        );

        match self.ensure_connected() {
            Ok(stream) => {
                if let Err(e) = stream.write_all(msg.as_bytes()) {
                    warn!("failed to send to kanata: {e}, will reconnect");
                    self.stream = None;
                } else {
                    debug!("sent to kanata: {msg}");
                }
            }
            Err(e) => {
                warn!("kanata connection failed: {e}");
                self.stream = None;
            }
        }
    }
}
