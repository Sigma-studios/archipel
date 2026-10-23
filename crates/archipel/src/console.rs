//! A server's console: what it prints, and what archipel says about it, kept in
//! memory for the web UI and appended to `<data>/logs/<id>.log`.

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::broadcast;

/// Lines kept in memory.
const KEEP: usize = 2000;
/// The log file is rotated past this size, the previous one kept as `.1`.
const ROTATE_AT: u64 = 5 * 1024 * 1024;

#[derive(Clone)]
pub struct Console(Arc<Inner>);

struct Inner {
    lines: Mutex<VecDeque<String>>,
    tx: broadcast::Sender<String>,
    file: PathBuf,
}

impl Console {
    pub fn new(file: PathBuf) -> Self {
        // Pick up where the last run left off.
        let mut lines = VecDeque::with_capacity(KEEP);
        if let Ok(text) = std::fs::read_to_string(&file) {
            for line in text
                .lines()
                .rev()
                .take(KEEP / 2)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                lines.push_back(line.to_string());
            }
        }
        let (tx, _) = broadcast::channel(1024);
        Console(Arc::new(Inner {
            lines: Mutex::new(lines),
            tx,
            file,
        }))
    }

    /// A line the server printed.
    pub fn line(&self, line: String) {
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        {
            let mut lines = self.0.lines.lock();
            if lines.len() == KEEP {
                lines.pop_front();
            }
            lines.push_back(line.clone());
        }
        self.append(&line);
        let _ = self.0.tx.send(line);
    }

    /// Something archipel says about the server.
    pub fn system(&self, message: String) {
        self.line(format!("[archipel] {message}"));
    }

    /// The last `n` lines, and new ones from then on.
    pub fn follow(&self, n: usize) -> (Vec<String>, broadcast::Receiver<String>) {
        let lines = self.0.lines.lock();
        // Subscribed under the lock: no line is lost nor sent twice.
        let rx = self.0.tx.subscribe();
        let skip = lines.len().saturating_sub(n);
        (lines.iter().skip(skip).cloned().collect(), rx)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.0.tx.subscribe()
    }

    fn append(&self, line: &str) {
        let path = &self.0.file;
        if std::fs::metadata(path).is_ok_and(|m| m.len() > ROTATE_AT) {
            let _ = std::fs::rename(path, path.with_extension("log.1"));
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}
