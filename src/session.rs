use crate::graph::{self, Sample};
use crate::proxy::{self, Level, Limits, PerDirection, Stats};
use std::{
    collections::VecDeque,
    sync::{Arc, atomic::Ordering},
    thread::JoinHandle,
    time::Instant,
};
use tokio::sync::watch;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Offline,
    Starting,
    Listening,
    Stopping,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Offline => "Offline",
            Self::Starting => "Starting",
            Self::Listening => "Listening",
            Self::Stopping => "Stopping",
        }
    }
}

/// One run of the proxy on its own thread. It keeps its stats and traffic
/// history after the thread ends, so the UI can still show the last run.
pub struct Session {
    shutdown: watch::Sender<bool>,
    limits: watch::Sender<Limits>,
    thread: Option<JoinHandle<()>>,
    stopping: bool,
    pub stats: Arc<Stats>,
    pub samples: VecDeque<Sample>,
    pub totals: PerDirection<u64>,
    sampled_at: Instant,
    pub started: Instant,
}

impl Session {
    pub fn start(port: u16, target: String, limits: Limits) -> Self {
        let (limits_tx, limits_rx) = watch::channel(limits);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let started = Instant::now();
        let stats = Arc::new(Stats::default());
        stats.set_status(
            Level::Info,
            "Starting listener and resolving destination...",
        );
        let worker_stats = stats.clone();
        let thread = std::thread::spawn(move || {
            let stats = worker_stats;
            let result = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("Runtime error: {error}"))
                .and_then(|runtime| {
                    let served = runtime.block_on(proxy::serve(
                        port,
                        target,
                        limits_rx,
                        shutdown_rx,
                        stats.clone(),
                    ));
                    // A DNS lookup cannot be cancelled. Dropping the runtime would wait
                    // for it, and `Drop for Session` would freeze the UI until it ends.
                    runtime.shutdown_background();
                    served.map_err(|error| format!("Proxy error: {error}"))
                });
            if let Err(message) = result {
                stats.set_status(Level::Error, message);
            }
            stats.listening.store(false, Ordering::Relaxed);
        });
        Self {
            shutdown: shutdown_tx,
            limits: limits_tx,
            thread: Some(thread),
            stopping: false,
            stats,
            samples: VecDeque::new(),
            totals: PerDirection::default(),
            sampled_at: started,
            started,
        }
    }

    pub fn running(&self) -> bool {
        self.thread.is_some()
    }

    pub fn status(&self) -> Status {
        if !self.running() {
            Status::Offline
        } else if self.stopping {
            Status::Stopping
        } else if self.stats.listening.load(Ordering::Relaxed) {
            Status::Listening
        } else {
            Status::Starting
        }
    }

    pub fn stop(&mut self) {
        let _ = self.shutdown.send(true);
        self.stopping = true;
    }

    pub fn set_limits(&self, limits: Limits) {
        self.limits.send_if_modified(|current| {
            let changed = *current != limits;
            *current = limits;
            changed
        });
    }

    /// Joins the worker thread once it has finished on its own or after `stop`.
    pub fn reap(&mut self) {
        if !self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            return;
        }
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            self.stats.listening.store(false, Ordering::Relaxed);
            self.stats
                .set_status(Level::Error, "Proxy worker stopped unexpectedly.");
        }
    }

    /// Records the transfer rate since the last sample, at most every `SAMPLE_SECONDS`.
    pub fn sample(&mut self) {
        let elapsed = self.sampled_at.elapsed().as_secs_f64();
        if !self.running() || elapsed < graph::SAMPLE_SECONDS {
            return;
        }
        let totals = PerDirection {
            download: self.stats.transferred.download.load(Ordering::Relaxed),
            upload: self.stats.transferred.upload.load(Ordering::Relaxed),
        };
        let rate = |now: u64, before: u64| now.saturating_sub(before) as f64 / elapsed / 1024.0;
        let sample = Sample {
            at: self.started.elapsed().as_secs_f64(),
            kib_per_second: PerDirection {
                download: rate(totals.download, self.totals.download),
                upload: rate(totals.upload, self.totals.upload),
            },
        };
        graph::push_sample(&mut self.samples, sample);
        self.totals = totals;
        self.sampled_at = Instant::now();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
