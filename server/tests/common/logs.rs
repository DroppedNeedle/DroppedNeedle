//! Log capture for tests that assert on what the server logs.

use std::sync::{Arc, Mutex};

use tracing::Dispatch;
use tracing::level_filters::LevelFilter;
use tracing::subscriber::NoSubscriber;

/// Runs `run` and returns its value with the log lines this thread wrote
/// during it.
///
/// A second dispatch stays registered for the whole capture. With only the
/// capture registered, a log call site first hit on another test's thread
/// is cached as never enabled for the rest of the process, and the capture
/// misses its lines.
pub fn capture_logs<T>(run: impl FnOnce() -> T) -> (T, String) {
    let text = Arc::new(Mutex::new(Vec::new()));
    let _silent = Dispatch::new(NoSubscriber::default());
    let subscriber = tracing_subscriber::fmt()
        .with_writer({
            let text = text.clone();
            move || LogWriter(text.clone())
        })
        .with_max_level(LevelFilter::TRACE)
        .finish();
    let value = tracing::dispatcher::with_default(&Dispatch::new(subscriber), run);
    let text = String::from_utf8(text.lock().unwrap().clone()).unwrap();
    (value, text)
}

struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
