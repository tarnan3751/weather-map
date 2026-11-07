//! `/ws`: every open browser gets every hub event as a JSON text message.
//!
//! One task reads the hub, serializes each event once and hands the same
//! bytes to every connection.

use crate::routes::AppState;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use bytes::Bytes;
use chrono::Utc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use wx_core::{Hub, WsEvent};

/// How often each connection hears from the server when nothing else is happening.
const HEARTBEAT: Duration = Duration::from_secs(25);
/// "Try again later": tells the client to reconnect and resync.
const CLOSE_TRY_AGAIN: u16 = 1013;
/// "Going away": the server is shutting down.
const CLOSE_GOING_AWAY: u16 = 1001;
/// Events a connection may fall behind by before it is told to resync,
/// as many as the hub keeps.
const BACKLOG: usize = 1024;

#[derive(Clone)]
enum Out {
    Event(Utf8Bytes),
    /// The fan-out itself fell behind the hub: every connection must resync.
    Resync,
}

/// Hub events, serialized once, for every connection.
#[derive(Clone)]
pub struct Live {
    tx: broadcast::Sender<Out>,
}

impl Live {
    /// Start reading `hub`. Events sent after this returns all reach the
    /// connections, in order.
    pub fn spawn(hub: &Hub) -> Self {
        let (tx, _) = broadcast::channel(BACKLOG);
        let mut events = hub.subscribe();
        let out = tx.clone();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        let _ = out.send(Out::Event(to_text(&event)));
                    }
                    Err(RecvError::Lagged(_)) => {
                        let _ = out.send(Out::Resync);
                    }
                    Err(RecvError::Closed) => return,
                }
            }
        });
        Self { tx }
    }
}

fn to_text(event: &WsEvent) -> Utf8Bytes {
    serde_json::to_string(event)
        .expect("events serialize")
        .into()
}

pub async fn handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| client(socket, state))
}

struct Connected<'a>(&'a AtomicUsize);

impl<'a> Connected<'a> {
    fn new(count: &'a AtomicUsize) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        Self(count)
    }
}

impl Drop for Connected<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn close(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}

async fn client(mut socket: WebSocket, state: AppState) {
    // Subscribe before greeting so nothing published in between is missed.
    let mut events = state.live.tx.subscribe();
    let _connected = Connected::new(&state.clients);
    let hello = WsEvent::Hello {
        server_time: Utc::now(),
    };
    if socket.send(Message::Text(to_text(&hello))).await.is_err() {
        return;
    }
    // Pings keep idle connections alive through proxies and reveal dead peers
    // to the server. Browsers do not show pings to page scripts, so each one is
    // paired with a heartbeat event the client can see.
    let mut ping = tokio::time::interval(HEARTBEAT);
    ping.tick().await;

    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(Out::Event(text)) => {
                    if socket.send(Message::Text(text)).await.is_err() {
                        return;
                    }
                }
                // This client fell too far behind to catch up event by event.
                // Closing makes it reconnect and refetch, which is the resync path.
                Ok(Out::Resync) | Err(RecvError::Lagged(_)) => {
                    close(&mut socket, CLOSE_TRY_AGAIN, "resync").await;
                    return;
                }
                Err(RecvError::Closed) => return,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
            _ = ping.tick() => {
                let beat = WsEvent::Heartbeat { server_time: Utc::now() };
                if socket.send(Message::Ping(Bytes::new())).await.is_err()
                    || socket.send(Message::Text(to_text(&beat))).await.is_err()
                {
                    return;
                }
            }
            _ = state.cancel.cancelled() => {
                close(&mut socket, CLOSE_GOING_AWAY, "server shutting down").await;
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::tests::hrrr_temp;
    use futures::StreamExt;

    fn timeline_event(layer: &str, n: usize, part: &wx_core::Part) -> WsEvent {
        sized_event(layer, n, part, 6)
    }

    /// A `timeline` event changing `frames` frames of eight parts each.
    fn sized_event(layer: &str, n: usize, part: &wx_core::Part, frames: usize) -> WsEvent {
        let frames = (0..frames)
            .map(|k| wx_core::Frame {
                valid: part.valid + chrono::Duration::minutes(10 * (n * frames + k) as i64),
                kind: wx_core::FrameKind::Forecast,
                parts: vec![part.id.clone(); 8],
            })
            .collect();
        WsEvent::Timeline {
            layer: layer.to_string(),
            parts: vec![part.clone(); 2],
            frames,
            removed: vec![part.valid],
        }
    }

    #[tokio::test]
    async fn events_are_serialized_once() {
        let (_, _, part) = hrrr_temp().await;
        let hub = Hub::new();
        let live = Live::spawn(&hub);
        let mut a = live.tx.subscribe();
        let mut b = live.tx.subscribe();
        let event = timeline_event("temp", 0, &part);
        hub.send(event.clone());
        let (Ok(Out::Event(x)), Ok(Out::Event(y))) = (a.recv().await, b.recv().await) else {
            panic!("an event");
        };
        assert_eq!(x.as_str(), serde_json::to_string(&event).unwrap());
        // The very same bytes for both connections.
        assert_eq!(x.as_str().as_ptr(), y.as_str().as_ptr());
    }

    /// The connection loop as it was: every connection serialized every
    /// event itself. Kept to measure against.
    async fn per_connection(mut socket: WebSocket, hub: Hub) {
        let mut events = hub.subscribe();
        while let Ok(event) = events.recv().await {
            let json = serde_json::to_string(&*event).expect("events serialize");
            if socket.send(Message::Text(json.into())).await.is_err() {
                return;
            }
        }
    }

    fn cpu_seconds() -> f64 {
        let out = std::process::Command::new("ps")
            .args(["-o", "time=", "-p", &std::process::id().to_string()])
            .output()
            .unwrap();
        // [[hh:]mm:]ss.cc
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .split(':')
            .fold(0.0, |acc, f| acc * 60.0 + f.parse::<f64>().unwrap_or(0.0))
    }

    /// `cargo test --release -p wx-server fan_out_burst -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement"]
    async fn fan_out_burst() {
        use axum::routing::get;
        let clients = 200;
        let (_, catalog, part) = hrrr_temp().await;
        let hub = Hub::new();
        let state = crate::routes::tests::state(catalog, &hub);
        let old_hub = hub.clone();
        let app = axum::Router::new()
            .route("/ws", get(handler))
            .route(
                "/ws-old",
                get(move |ws: WebSocketUpgrade| {
                    let hub = old_hub.clone();
                    async move { ws.on_upgrade(move |s| per_connection(s, hub)) }
                }),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for (burst, frames, path) in [
            (500, 6, "ws-old"),
            (500, 6, "ws"),
            (500, 6, "ws-old"),
            (500, 6, "ws"),
            (100, 120, "ws-old"),
            (100, 120, "ws"),
            (100, 120, "ws-old"),
            (100, 120, "ws"),
        ] {
            let events: Vec<WsEvent> = (0..burst)
                .map(|n| sized_event("temp", n, &part, frames))
                .collect();
            let size = serde_json::to_string(&events[0]).unwrap().len();
            let mut sockets = Vec::new();
            for _ in 0..clients {
                let (s, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/{path}"))
                    .await
                    .unwrap();
                sockets.push(s);
            }
            // The new loop greets first; wait until every connection is subscribed.
            tokio::time::sleep(Duration::from_millis(300)).await;
            let (started, cpu) = (std::time::Instant::now(), cpu_seconds());
            let readers: Vec<_> = sockets
                .into_iter()
                .map(|mut s| {
                    tokio::spawn(async move {
                        let mut timelines = 0;
                        while timelines < burst {
                            match s.next().await {
                                Some(Ok(m))
                                    if m.to_text().is_ok_and(|t| t.contains("\"timeline\"")) =>
                                {
                                    timelines += 1
                                }
                                Some(Ok(_)) => {}
                                _ => panic!("connection lost"),
                            }
                        }
                    })
                })
                .collect();
            for e in &events {
                hub.send(e.clone());
            }
            for r in readers {
                r.await.unwrap();
            }
            let (t, cpu) = (started.elapsed(), cpu_seconds() - cpu);
            println!(
                "/{path}: {clients} clients x {burst} events of {size} bytes delivered in {:.0} ms \
                 ({:.0} messages/s), {cpu:.2} s CPU with the clients",
                t.as_secs_f64() * 1e3,
                (clients * burst) as f64 / t.as_secs_f64()
            );
        }
    }
}
