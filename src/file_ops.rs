//! Async file picking operations with cross-platform support.
//!
//! Uses channel-based communication to bridge async file dialogs
//! with egui's synchronous update loop.

use eframe::egui;
use std::sync::mpsc::{channel, Receiver, Sender};

/// Result of a file pick operation.
#[derive(Clone)]
pub struct FilePickResult {
    pub file_name: String,
    pub file_size: u64,
    pub file_data: Vec<u8>,
}

/// Channel-based file picker for async file dialog integration.
///
/// File dialogs are async but egui's update() is synchronous.
/// This struct provides a channel to pass results from the async
/// file picker task back to the UI thread.
pub struct FilePickerChannel {
    sender: Sender<Option<FilePickResult>>,
    receiver: Receiver<Option<FilePickResult>>,
}

impl Default for FilePickerChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl FilePickerChannel {
    pub fn new() -> Self {
        let (sender, receiver) = channel();
        Self { sender, receiver }
    }

    /// Spawns an async file picker dialog.
    ///
    /// On native: spawns a new thread using pollster to block on the async dialog.
    /// On WASM: uses wasm_bindgen_futures::spawn_local.
    ///
    /// When the dialog completes (or is cancelled), the result is sent through
    /// the channel and ctx.request_repaint() is called to trigger a UI update.
    pub fn pick_file(&self, ctx: egui::Context) {
        let sender = self.sender.clone();

        #[cfg(not(target_arch = "wasm32"))]
        {
            std::thread::spawn(move || {
                let result = pollster::block_on(async_pick_file());
                let _ = sender.send(result);
                ctx.request_repaint();
            });
        }

        #[cfg(target_arch = "wasm32")]
        {
            wasm_bindgen_futures::spawn_local(async move {
                let result = async_pick_file().await;
                let _ = sender.send(result);
                ctx.request_repaint();
            });
        }
    }

    /// Non-blocking check for a completed file pick.
    ///
    /// Returns Some(Some(result)) if a file was picked,
    /// Some(None) if the dialog was cancelled,
    /// None if no result is ready yet.
    pub fn try_recv(&self) -> Option<Option<FilePickResult>> {
        self.receiver.try_recv().ok()
    }
}

/// Async file picker implementation using rfd.
async fn async_pick_file() -> Option<FilePickResult> {
    let file = rfd::AsyncFileDialog::new()
        .set_title("Select NEXRAD File")
        .pick_file()
        .await?;

    let file_name = file.file_name();
    let file_data = file.read().await;
    let file_size = file_data.len() as u64;

    Some(FilePickResult {
        file_name,
        file_size,
        file_data,
    })
}
