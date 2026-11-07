//! Live updates. Everything the server pushes to open browsers is a
//! [`WsEvent`], sent as one JSON text message per event on `/ws`.

use crate::feed::FeedHealth;
use crate::frame::{Frame, Part};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsEvent {
    /// First message on every connection.
    Hello { server_time: DateTime<Utc> },
    /// Sent to each connection every 25 seconds so a client can tell a quiet
    /// connection from a dead one, and keep its clock in step with the server.
    Heartbeat { server_time: DateTime<Utc> },
    /// A layer's timeline changed. `frames` are new or changed time steps
    /// (a frame is identified by its `valid`), `removed` the time steps that
    /// no longer exist, and `parts` describes every part named by `frames`
    /// that the timeline did not name before. Parts no frame names any more
    /// are simply forgotten; their tiles stay fetchable for four minutes.
    Timeline {
        layer: String,
        parts: Vec<Part>,
        frames: Vec<Frame>,
        removed: Vec<DateTime<Utc>>,
    },
    /// The set of active alerts changed and is now at `revision`. Clients
    /// switch to that revision's vector tiles and refetch any alert lists
    /// they show. The ids say which alerts appeared on the map, changed, or
    /// left it.
    Alerts {
        revision: i64,
        added: Vec<String>,
        updated: Vec<String>,
        removed: Vec<String>,
    },
    /// Fresh station observations are available at `/api/stations`.
    Stations {
        updated: DateTime<Utc>,
        count: usize,
    },
    /// A feed changed state (for the status page).
    Feed { feed: FeedHealth },
}

/// Fan-out point for live updates. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Hub {
    tx: broadcast::Sender<Arc<WsEvent>>,
}

impl Hub {
    pub fn new() -> Self {
        Self::with_capacity(1024)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Send to every current subscriber. Having no subscribers is not an error.
    pub fn send(&self, event: WsEvent) {
        let _ = self.tx.send(Arc::new(event));
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<WsEvent>> {
        self.tx.subscribe()
    }
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}
