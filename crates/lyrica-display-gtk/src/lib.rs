use anyhow::Result;
use tokio::sync::watch;

use lyrica_core::display::{DisplayBackend, DisplayState};

/// GTK4 transparent overlay display backend.
/// TODO: Implement in Phase 4.
pub struct GtkDisplay;

impl GtkDisplay {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl DisplayBackend for GtkDisplay {
    async fn run(&mut self, _state_rx: watch::Receiver<DisplayState>) -> Result<()> {
        anyhow::bail!("GTK display not yet implemented")
    }
}
