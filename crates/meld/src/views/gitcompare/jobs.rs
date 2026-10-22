//! Background jobs of a Git Compare tab (the role of `meld/ailens/worker.py`
//! in `gitcompare.py`'s `_run`).
//!
//! Every git and Claude Code call blocks, so each job runs on a thread of
//! its own and reports back through a [`Task`] stream: progress texts while
//! it runs, then its result. Jobs have names; starting an exclusive job
//! cancels the running job of the same name, and a result only counts when
//! it comes from the newest job of its name ([`Jobs::finish`]).

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use iced::Task;
use iced::futures::channel::mpsc;
use meld_ailens::{CancelToken, Error};

/// What a running job sends back.
pub enum Event<T> {
    Progress(String),
    Done(Result<T, Error>),
}

/// A value that only needs to travel inside a message: cheap to clone,
/// printed by type name.
pub struct Opaque<T>(pub Arc<T>);

impl<T> Clone for Opaque<T> {
    fn clone(&self) -> Self {
        Opaque(self.0.clone())
    }
}

impl<T> fmt::Debug for Opaque<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Opaque<{}>", std::any::type_name::<T>())
    }
}

impl<T> std::ops::Deref for Opaque<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// Identifies one run of a named job.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ticket {
    pub name: String,
    pub generation: u64,
}

struct Running {
    generation: u64,
    cancel: CancelToken,
}

/// The tab's running jobs.
#[derive(Default)]
pub struct Jobs {
    running: HashMap<String, Running>,
    next: u64,
}

impl Jobs {
    /// Registers a new run of `name`, cancelling the previous one when
    /// `exclusive`. A non-exclusive job keeps running alongside; its name
    /// should then be unique (e.g. per card).
    pub fn start(&mut self, name: &str, exclusive: bool) -> (Ticket, CancelToken) {
        self.next += 1;
        if exclusive && let Some(old) = self.running.remove(name) {
            old.cancel.cancel();
        }
        let cancel = CancelToken::new();
        self.running.insert(
            name.to_string(),
            Running {
                generation: self.next,
                cancel: cancel.clone(),
            },
        );
        (
            Ticket {
                name: name.to_string(),
                generation: self.next,
            },
            cancel,
        )
    }

    /// Whether `ticket` is the newest run of its job (its progress and
    /// result still count).
    pub fn is_current(&self, ticket: &Ticket) -> bool {
        self.running
            .get(&ticket.name)
            .is_some_and(|r| r.generation == ticket.generation)
    }

    /// Takes a finished job off the list; false when its result is stale.
    pub fn finish(&mut self, ticket: &Ticket) -> bool {
        if self.is_current(ticket) {
            self.running.remove(&ticket.name);
            true
        } else {
            false
        }
    }

    pub fn is_running(&self, name: &str) -> bool {
        self.running.contains_key(name)
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.running.is_empty()
    }

    /// Escape: cancel everything (the processes get killed).
    pub fn cancel_all(&mut self) {
        for (_, running) in self.running.drain() {
            running.cancel.cancel();
        }
    }
}

impl Drop for Jobs {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

/// Runs `work` on a thread of its own; the task yields its progress and
/// then its result. A panic in `work` becomes an error.
pub fn spawn<T, W>(cancel: CancelToken, work: W) -> Task<Event<T>>
where
    T: Send + 'static,
    W: FnOnce(&CancelToken, &dyn Fn(&str)) -> Result<T, Error> + Send + 'static,
{
    let (sender, receiver) = mpsc::unbounded();
    let spawned = std::thread::Builder::new()
        .name("meld-gitcompare".into())
        .spawn(move || {
            let progress_sender = sender.clone();
            let progress = move |text: &str| {
                let _ = progress_sender.unbounded_send(Event::Progress(text.to_string()));
            };
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&cancel, &progress)))
                    .unwrap_or_else(|panic| {
                        let text = panic
                            .downcast_ref::<&str>()
                            .map(|s| s.to_string())
                            .or_else(|| panic.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "unknown error".into());
                        log::error!("Git Compare job panicked: {text}");
                        Err(Error::msg(tr!("Internal error: {text}", text = text)))
                    });
            let _ = sender.unbounded_send(Event::Done(result));
        });
    if let Err(err) = spawned {
        return Task::done(Event::Done(Err(Error::msg(tr!(
            "Couldn’t start a thread: {err}",
            err = err
        )))));
    }
    Task::stream(receiver)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_jobs_replace_each_other() {
        let mut jobs = Jobs::default();
        let (first, first_cancel) = jobs.start("file", true);
        let (second, _) = jobs.start("file", true);
        assert!(first_cancel.is_cancelled());
        assert!(!jobs.is_current(&first));
        assert!(!jobs.finish(&first));
        assert!(jobs.finish(&second));
        assert!(jobs.is_empty());
    }

    #[test]
    fn cancel_all_cancels_every_token() {
        let mut jobs = Jobs::default();
        let (_, a) = jobs.start("explain-1", false);
        let (_, b) = jobs.start("explain-2", false);
        jobs.cancel_all();
        assert!(a.is_cancelled() && b.is_cancelled());
        assert!(!jobs.is_running("explain-1"));
    }
}
