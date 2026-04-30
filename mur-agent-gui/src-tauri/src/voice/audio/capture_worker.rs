//! Dedicated-thread capture worker.
//!
//! `cpal::Stream` is `!Send` on macOS — Core Audio callbacks must run
//! on the same thread that built the stream. So we can't park a
//! `CaptureHandle` in `tauri::State` (Tauri commands hop tokio
//! workers). Instead, this worker owns an OS thread; `start()` spawns
//! it, the thread builds the cpal stream, and parks on a stop
//! channel. `stop()` signals the thread, joins it, and returns the
//! drained samples.
//!
//! All public state (`stop_tx`, `join`) lives in `Send + Sync` types
//! (channels + JoinHandle), so the worker itself is safe in
//! `tokio::sync::Mutex<CaptureWorker>` inside Tauri state.

use anyhow::{Result, bail};
use std::sync::mpsc;
use std::thread::JoinHandle;

pub struct CaptureWorker {
    join: Option<JoinHandle<Vec<i16>>>,
    stop_tx: Option<mpsc::Sender<()>>,
}

impl CaptureWorker {
    pub const fn new() -> Self {
        Self {
            join: None,
            stop_tx: None,
        }
    }

    /// True if a capture thread is currently running.
    pub fn is_running(&self) -> bool {
        self.join.is_some()
    }

    /// Spawn the capture thread. The thread builds the cpal stream,
    /// blocks on `stop_rx.recv()`, and on stop drains the
    /// `CaptureBuffer` into a `Vec<i16>` returned via `JoinHandle`.
    pub fn start(&mut self) -> Result<()> {
        if self.join.is_some() {
            bail!("capture already running");
        }
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let join = std::thread::Builder::new()
            .name("voice-capture".into())
            .spawn(move || -> Vec<i16> {
                let handle = match super::start_capture() {
                    Ok(h) => h,
                    Err(e) => {
                        tracing::warn!(error = %e, "capture thread: start_capture failed");
                        return Vec::new();
                    }
                };
                // Park until stop signal arrives. `recv()` returns Err
                // only if the sender is dropped, which we treat as
                // "stop now" (e.g., the worker was dropped without
                // explicit stop()).
                let _ = stop_rx.recv();
                let samples = handle.buffer.drain();
                // Dropping `handle` ends the cpal stream cleanly.
                drop(handle);
                samples
            })?;
        self.stop_tx = Some(stop_tx);
        self.join = Some(join);
        Ok(())
    }

    /// Signal the capture thread to stop and return the captured samples.
    /// Idempotent: returns an error if no thread is running.
    pub fn stop(&mut self) -> Result<Vec<i16>> {
        let Some(stop) = self.stop_tx.take() else {
            bail!("no active capture to stop");
        };
        let Some(join) = self.join.take() else {
            bail!("no capture thread");
        };
        // Send is best-effort: if the thread already exited, drop the
        // channel and just join.
        let _ = stop.send(());
        join.join()
            .map_err(|_| anyhow::anyhow!("capture thread panicked"))
    }
}

impl Default for CaptureWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CaptureWorker {
    fn drop(&mut self) {
        // Signal stop on drop so a background capture doesn't outlive
        // the worker. Best-effort: ignore stop / join errors.
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_worker_is_not_running() {
        let w = CaptureWorker::new();
        assert!(!w.is_running());
    }

    #[test]
    fn stop_without_start_errors() {
        let mut w = CaptureWorker::new();
        let r = w.stop();
        assert!(r.is_err());
    }
}
