//! Ordered atomic settings writes off the UI thread. Dropping drains pending saves.
use std::{path::PathBuf, sync::mpsc, thread};

pub(super) struct Writer {
    jobs: Option<mpsc::Sender<Vec<u8>>>,
    errors: mpsc::Receiver<String>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Writer {
    pub fn new(path: PathBuf) -> Self {
        let (jobs, input) = mpsc::channel::<Vec<u8>>();
        let (errors, output) = mpsc::channel();
        let thread = thread::spawn(move || {
            while let Ok(bytes) = input.recv() {
                if let Err(e) = crate::layouts::write_atomic(&path, &bytes) {
                    let _ = errors.send(format!("Could not save settings: {e}"));
                }
            }
        });
        Self { jobs: Some(jobs), errors: output, thread: Some(thread) }
    }
    pub fn save(&self, bytes: Vec<u8>) {
        let _ = self.jobs.as_ref().expect("settings writer").send(bytes);
    }
    pub fn error(&self) -> Option<String> {
        self.errors.try_iter().last()
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        self.jobs.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordered_saves_are_atomic_and_drain_on_shutdown() {
        let dir = std::env::temp_dir().join(format!("terminal-settings-writer-{}", std::process::id()));
        let path = dir.join("settings.json");
        let writer = Writer::new(path.clone());
        for n in 0..100 {
            writer.save(format!("{{\"version\":{n}}}").into_bytes());
        }
        drop(writer);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"version\":99}");
        assert!(!path.with_extension("tmp").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
