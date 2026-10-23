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
    lines: Mutex<Lines>,
    tx: broadcast::Sender<String>,
    /// Wakes those waiting for new lines.
    more: tokio::sync::Notify,
    file: PathBuf,
}

/// The lines kept, numbered from 1 since archipel started.
struct Lines {
    kept: VecDeque<String>,
    /// The number of the first one kept.
    first: u64,
}

impl Lines {
    /// The number the next line gets.
    fn end(&self) -> u64 {
        self.first + self.kept.len() as u64
    }
}

/// Lines a first look (or one after a gap) shows.
const SHOWN_FIRST: usize = 500;

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
            lines: Mutex::new(Lines {
                kept: lines,
                first: 1,
            }),
            tx,
            more: tokio::sync::Notify::new(),
            file,
        }))
    }

    /// A line the server printed.
    pub fn line(&self, line: String) {
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        {
            let mut lines = self.0.lines.lock();
            if lines.kept.len() == KEEP {
                lines.kept.pop_front();
                lines.first += 1;
            }
            lines.kept.push_back(line.clone());
        }
        self.append(&line);
        let _ = self.0.tx.send(line);
        self.0.more.notify_waiters();
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
        let skip = lines.kept.len().saturating_sub(n);
        (lines.kept.iter().skip(skip).cloned().collect(), rx)
    }

    /// The lines after line `after`, the number to ask after next, and whether
    /// they replace what was shown (`after` is 0, or no longer kept, or from
    /// before archipel restarted).
    pub fn since(&self, after: u64) -> (Vec<String>, u64, bool) {
        let lines = self.0.lines.lock();
        let end = lines.end();
        if after == 0 && lines.kept.is_empty() {
            // Nothing yet: wait for the first line like for any other.
            return (vec![], 0, false);
        }
        if after == 0 || after + 1 < lines.first || after >= end {
            let skip = lines.kept.len().saturating_sub(SHOWN_FIRST);
            let shown = lines.kept.iter().skip(skip).cloned().collect();
            return (shown, end - 1, true);
        }
        let skip = (after + 1 - lines.first) as usize;
        (
            lines.kept.iter().skip(skip).cloned().collect(),
            end - 1,
            false,
        )
    }

    /// Like [`Console::since`], waiting up to `wait` for a line when there is none.
    pub async fn wait_since(
        &self,
        after: u64,
        wait: std::time::Duration,
    ) -> (Vec<String>, u64, bool) {
        let notified = self.0.more.notified();
        let now = self.since(after);
        if !now.0.is_empty() || now.2 {
            return now;
        }
        let _ = tokio::time::timeout(wait, notified).await;
        self.since(after)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_lines() {
        let dir = std::env::temp_dir().join(format!("archipel-console-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let console = Console::new(dir.join("1.log"));
        let (shown, next, reset) = console.since(0);
        assert!(shown.is_empty() && !reset && next == 0);
        console.line("a".into());
        console.line("b".into());
        // A first look: what there is, replacing nothing yet shown.
        assert_eq!(
            console.since(next),
            (vec!["a".to_string(), "b".to_string()], 2, true)
        );
        assert_eq!(console.since(1), (vec!["b".to_string()], 2, false));
        assert_eq!(console.since(2), (vec![], 2, false));
        // Ahead of what exists: archipel restarted, everything is shown again.
        assert!(console.since(99).2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
