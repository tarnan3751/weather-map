//! Limits on concurrent work that every source shares, handed out fairly.
//!
//! A [`FairPool`] is a counting semaphore whose waiters queue per lane (one
//! lane per supervised task) and are served round-robin across lanes. A task
//! with fifty downloads queued therefore delays a task with one by at most one
//! permit per other waiting lane, never by the whole queue: a model run that
//! is backfilling cannot starve the two-minute radar.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::oneshot;

#[derive(Default)]
struct State {
    free: usize,
    /// Lanes that have waiters, in the order they are served next.
    order: VecDeque<Arc<str>>,
    /// Waiters per lane, oldest first. A lane is here exactly when it is in
    /// `order`.
    waiting: HashMap<Arc<str>, VecDeque<oneshot::Sender<Permit>>>,
}

impl State {
    /// The next live waiter in round-robin order. Waiters that gave up (their
    /// acquire was dropped) are discarded on the way.
    fn next_waiter(&mut self) -> Option<oneshot::Sender<Permit>> {
        while let Some(lane) = self.order.pop_front() {
            let Some(queue) = self.waiting.get_mut(&lane) else {
                continue;
            };
            let mut found = None;
            while let Some(tx) = queue.pop_front() {
                if !tx.is_closed() {
                    found = Some(tx);
                    break;
                }
            }
            if queue.is_empty() {
                self.waiting.remove(&lane);
            } else {
                self.order.push_back(lane);
            }
            if found.is_some() {
                return found;
            }
        }
        None
    }
}

/// A counting semaphore served round-robin across lanes. Cheap to clone.
///
/// Invariant: while a permit is free no live waiter exists, because a
/// returned permit goes to a waiter before it is counted as free, and a
/// waiter only queues when none is free.
#[derive(Clone)]
pub struct FairPool {
    inner: Arc<Mutex<State>>,
}

/// One unit of a [`FairPool`], returned when dropped.
pub struct Permit {
    pool: Option<Arc<Mutex<State>>>,
}

impl std::fmt::Debug for Permit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Permit")
    }
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    // Every update leaves the state consistent, so a holder that panicked
    // does not make it unusable.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Drop for Permit {
    fn drop(&mut self) {
        let Some(pool) = self.pool.take() else { return };
        loop {
            let tx = {
                let mut s = lock(&pool);
                match s.next_waiter() {
                    Some(tx) => tx,
                    None => {
                        s.free += 1;
                        return;
                    }
                }
            };
            // Sent outside the lock. A waiter that gave up between the check
            // and the send hands the permit back, and the next one gets it.
            match tx.send(Permit {
                pool: Some(pool.clone()),
            }) {
                Ok(()) => return,
                Err(mut back) => back.pool = None,
            }
        }
    }
}

impl FairPool {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            inner: Arc::new(Mutex::new(State {
                free: capacity,
                ..State::default()
            })),
        }
    }

    /// Wait for a permit, queued behind other waiters of `lane` and served
    /// in turn with the other lanes. Dropping the future gives up the place.
    pub async fn acquire(&self, lane: &str) -> Permit {
        let rx = {
            let mut s = lock(&self.inner);
            if s.free > 0 {
                s.free -= 1;
                return Permit {
                    pool: Some(self.inner.clone()),
                };
            }
            let (tx, rx) = oneshot::channel();
            let lane: Arc<str> = lane.into();
            match s.waiting.get_mut(&lane) {
                Some(queue) => queue.push_back(tx),
                None => {
                    s.waiting.insert(lane.clone(), VecDeque::from([tx]));
                    s.order.push_back(lane);
                }
            }
            rx
        };
        // The pool (held by `self`) outlives this wait, so the sender is
        // never dropped unsent while the receiver waits.
        rx.await.unwrap_or(Permit { pool: None })
    }

    /// Permits free right now.
    #[cfg(test)]
    pub fn available(&self) -> usize {
        lock(&self.inner).free
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn lanes_take_turns() {
        let pool = FairPool::new(1);
        let held = pool.acquire("gfs").await;
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        // Three downloads of a model run queue first, then one radar file.
        for name in ["gfs-1", "gfs-2", "gfs-3", "radar-1"] {
            let (pool, order) = (pool.clone(), order.clone());
            let lane = name.split('-').next().unwrap().to_string();
            tasks.push(tokio::spawn(async move {
                let _p = pool.acquire(&lane).await;
                order.lock().unwrap().push(name);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        drop(held);
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(
            *order.lock().unwrap(),
            ["gfs-1", "radar-1", "gfs-2", "gfs-3"]
        );
        assert_eq!(pool.available(), 1);
    }

    #[tokio::test]
    async fn a_waiter_that_gives_up_loses_nothing() {
        let pool = FairPool::new(2);
        let a = pool.acquire("x").await;
        let b = pool.acquire("x").await;
        assert_eq!(pool.available(), 0);
        // Gives up while queued.
        let gave_up = tokio::time::timeout(Duration::from_millis(10), pool.acquire("y")).await;
        assert!(gave_up.is_err());
        let waiting = {
            let pool = pool.clone();
            tokio::spawn(async move { pool.acquire("z").await })
        };
        tokio::time::sleep(Duration::from_millis(5)).await;
        // The dead waiter is skipped; the live one gets the permit.
        drop(a);
        let c = waiting.await.unwrap();
        drop(b);
        drop(c);
        assert_eq!(pool.available(), 2);
    }
}
