//! Runs blocking jobs on threads and hands their results back to the UI
//! (port of `worker.py`)
//!
//! Where the Python version posted results with `GLib.idle_add`, this one
//! hands each finished job's callback to a [`Dispatch`] function: the app
//! typically pushes it into a channel it drains on its own thread (or an
//! iced subscription), and runs it there.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};
use crate::process::CancelToken;

/// A finished job's callback, to be run where the UI state lives
pub type Delivery = Box<dyn FnOnce() + Send>;

/// Hands a [`Delivery`] to the UI thread
pub type Dispatch = Arc<dyn Fn(Delivery) + Send + Sync>;

type Job = Box<dyn FnOnce() + Send>;

/// A [`Dispatch`] that sends deliveries to a channel; run each one received
pub fn channel_dispatch() -> (Dispatch, mpsc::Receiver<Delivery>) {
    let (sender, receiver) = mpsc::channel::<Delivery>();
    let sender = Mutex::new(sender);
    let dispatch: Dispatch = Arc::new(move |delivery| {
        let _ = lock(&sender).send(delivery);
    });
    (dispatch, receiver)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Default)]
struct State {
    /// Exclusive jobs by name: their serial and token
    jobs: HashMap<String, (u64, CancelToken)>,
    /// Tokens of jobs not yet delivered, by serial
    tokens: HashMap<u64, CancelToken>,
    serial: u64,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    dispatch: Dispatch,
}

impl Shared {
    fn forget(&self, name: &str, serial: u64, exclusive: bool) {
        let mut state = lock(&self.state);
        state.tokens.remove(&serial);
        if exclusive && state.jobs.get(name).is_some_and(|job| job.0 == serial) {
            state.jobs.remove(name);
        }
    }

    fn current(&self, name: &str, serial: u64, token: &CancelToken, exclusive: bool) -> bool {
        let state = lock(&self.state);
        if state.closed || token.is_cancelled() {
            return false;
        }
        if exclusive {
            return state.jobs.get(name).is_some_and(|job| job.0 == serial);
        }
        true
    }
}

/// A small pool of threads for one tab
///
/// Each job gets its own [`CancelToken`]. Submitting an `exclusive` job
/// cancels the running one of the same name, whose result is then never
/// delivered, so callbacks only ever see the newest answer. After
/// [`Worker::shutdown`] no callback runs at all.
pub struct Worker {
    shared: Arc<Shared>,
    queue: Mutex<Option<mpsc::Sender<Job>>>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl Worker {
    pub fn new(max_workers: usize, dispatch: Dispatch) -> Self {
        let (sender, receiver) = mpsc::channel::<Job>();
        let receiver = Arc::new(Mutex::new(receiver));
        let threads = (0..max_workers.max(1))
            .map(|index| {
                let receiver = receiver.clone();
                std::thread::Builder::new()
                    .name(format!("meld-ailens-{index}"))
                    .spawn(move || {
                        loop {
                            let job = lock(&receiver).recv();
                            match job {
                                Ok(job) => job(),
                                Err(_) => return,
                            }
                        }
                    })
                    .expect("can spawn a worker thread")
            })
            .collect();
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                dispatch,
            }),
            queue: Mutex::new(Some(sender)),
            threads: Mutex::new(threads),
        }
    }

    /// Run `work` on a worker thread; `on_done` or `on_failed` is then
    /// dispatched, unless the job was cancelled or superseded meanwhile
    ///
    /// A job that fails with [`Error::Cancelled`] after its token fired is
    /// dropped silently.
    pub fn submit<T, W, D, F>(
        &self,
        name: &str,
        work: W,
        on_done: D,
        on_failed: F,
        exclusive: bool,
    ) -> CancelToken
    where
        T: Send + 'static,
        W: FnOnce(&CancelToken) -> Result<T> + Send + 'static,
        D: FnOnce(T) + Send + 'static,
        F: FnOnce(Error) + Send + 'static,
    {
        let token = CancelToken::new();
        let serial;
        {
            let mut state = lock(&self.shared.state);
            if state.closed {
                token.cancel();
                return token;
            }
            state.serial += 1;
            serial = state.serial;
            if exclusive {
                if let Some(previous) = state.jobs.get(name) {
                    previous.1.cancel();
                }
                state.jobs.insert(name.to_string(), (serial, token.clone()));
            }
            state.tokens.insert(serial, token.clone());
        }

        let shared = self.shared.clone();
        let job_token = token.clone();
        let job_name = name.to_string();
        let name = name.to_string();
        let job: Job = Box::new(move || {
            if job_token.is_cancelled() {
                shared.forget(&name, serial, exclusive);
                return;
            }
            // A panicking job fails like any other, and keeps its thread
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&job_token)))
                    .unwrap_or_else(|_| Err(Error::msg("The background job failed unexpectedly.")));
            let callback: Delivery = match result {
                Err(Error::Cancelled) if job_token.is_cancelled() => {
                    shared.forget(&name, serial, exclusive);
                    return;
                }
                Ok(value) => Box::new(move || on_done(value)),
                Err(err) => Box::new(move || on_failed(err)),
            };
            let deliver_shared = shared.clone();
            let deliver: Delivery = Box::new(move || {
                // Checked again on the UI thread: a newer job or shutdown
                // may have come in while this was queued
                if deliver_shared.current(&name, serial, &job_token, exclusive) {
                    callback();
                }
                deliver_shared.forget(&name, serial, exclusive);
            });
            (shared.dispatch)(deliver);
        });

        let sent = lock(&self.queue)
            .as_ref()
            .is_some_and(|queue| queue.send(job).is_ok());
        if !sent {
            // The pool shut down in between
            token.cancel();
            self.shared.forget(&job_name, serial, exclusive);
        }
        token
    }

    pub fn cancel(&self, name: &str) {
        let job = lock(&self.shared.state).jobs.remove(name);
        if let Some((_, token)) = job {
            token.cancel();
        }
    }

    pub fn cancel_all(&self) {
        let tokens: Vec<CancelToken> = {
            let mut state = lock(&self.shared.state);
            state.jobs.clear();
            state.tokens.values().cloned().collect()
        };
        for token in tokens {
            token.cancel();
        }
    }

    /// Whether any job is running or waiting for delivery
    pub fn busy(&self) -> bool {
        !lock(&self.shared.state).tokens.is_empty()
    }

    /// Names of the exclusive jobs still running
    pub fn running(&self) -> HashSet<String> {
        lock(&self.shared.state).jobs.keys().cloned().collect()
    }

    /// Cancel everything; no callback runs after this returns
    pub fn shutdown(&self) {
        lock(&self.shared.state).closed = true;
        self.cancel_all();
        // Closing the queue lets the threads exit once their job returns
        lock(&self.queue).take();
        lock(&self.threads).clear();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown();
    }
}
