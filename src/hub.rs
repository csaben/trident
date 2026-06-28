// The broker. A tiny HTTP + Server-Sent Events server that routes messages
// between trident sessions. Port-for-port the same protocol as the node
// hub.mjs so either implementation interoperates:
//   GET  /stream?name=<n>   register + receive (SSE)
//   GET  /roster            JSON list of connected names
//   POST /send              { from, to, content, meta } -> route to target(s)

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::{
    extract::{Query, State},
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
    Json, Router,
};
use futures::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::config;

type Tx = mpsc::UnboundedSender<String>;

#[derive(Clone)]
struct Hub {
    clients: Arc<Mutex<HashMap<String, Tx>>>,
}

pub async fn run() -> anyhow::Result<()> {
    let port = config::hub_port();
    let hub = Hub {
        clients: Arc::new(Mutex::new(HashMap::new())),
    };

    let app = Router::new()
        .route("/stream", get(stream))
        .route("/roster", get(roster))
        .route("/send", post(send))
        .route("/", get(|| async { "trident hub\n" }))
        .with_state(hub);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // A session may auto-start a hub that already exists; losing the
            // race for the port is expected and harmless.
            eprintln!("[trident-hub] port {port} already in use; a hub is already running");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    eprintln!("[trident-hub] listening on http://0.0.0.0:{port}");
    axum::serve(listener, app).await?;
    Ok(())
}

// --- helpers ---------------------------------------------------------------

fn unique_name(clients: &HashMap<String, Tx>, requested: &str) -> String {
    let base = match requested.trim() {
        "" => "session",
        s => s,
    };
    if !clients.contains_key(base) {
        return base.to_string();
    }
    let mut i = 2;
    loop {
        let candidate = format!("{base}-{i}");
        if !clients.contains_key(&candidate) {
            return candidate;
        }
        i += 1;
    }
}

fn broadcast_roster(hub: &Hub) {
    let clients = hub.clients.lock().unwrap();
    let names: Vec<&String> = clients.keys().collect();
    let msg = json!({ "type": "roster", "sessions": names }).to_string();
    for tx in clients.values() {
        let _ = tx.send(msg.clone());
    }
}

/// Removes a client and refreshes the roster when its SSE stream is dropped.
struct ClientGuard {
    name: String,
    hub: Hub,
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        let removed = {
            let mut clients = self.hub.clients.lock().unwrap();
            clients.remove(&self.name).is_some()
        };
        if removed {
            broadcast_roster(&self.hub);
            eprintln!("[trident-hub] - {}", self.name);
        }
    }
}

// --- handlers --------------------------------------------------------------

async fn stream(
    State(hub): State<Hub>,
    Query(q): Query<HashMap<String, String>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let requested = q.get("name").cloned().unwrap_or_default();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    let name = {
        let mut clients = hub.clients.lock().unwrap();
        let name = unique_name(&clients, &requested);
        clients.insert(name.clone(), tx.clone());
        name
    };
    eprintln!("[trident-hub] + {name}");

    // Tell the client its final name, then refresh everyone's roster.
    let _ = tx.send(json!({ "type": "registered", "name": name }).to_string());
    broadcast_roster(&hub);

    let guard = ClientGuard {
        name,
        hub: hub.clone(),
    };

    let body = async_stream::stream! {
        let _guard = guard; // dropped (→ deregister) when the client disconnects
        while let Some(msg) = rx.recv().await {
            yield Ok(Event::default().data(msg));
        }
    };

    Sse::new(body).keep_alive(KeepAlive::default())
}

async fn roster(State(hub): State<Hub>) -> Json<Value> {
    let clients = hub.clients.lock().unwrap();
    let names: Vec<&String> = clients.keys().collect();
    Json(json!({ "sessions": names }))
}

#[derive(Deserialize)]
struct SendBody {
    from: Option<String>,
    to: Option<String>,
    content: Option<String>,
    #[serde(default)]
    meta: Value,
}

async fn send(State(hub): State<Hub>, Json(body): Json<SendBody>) -> Json<Value> {
    let from = body.from.unwrap_or_else(|| "unknown".into());
    let to = body.to.unwrap_or_else(|| "all".into());
    let meta = if body.meta.is_null() {
        json!({})
    } else {
        body.meta
    };
    let out = json!({
        "type": "message",
        "from": from,
        "to": to,
        "content": body.content.unwrap_or_default(),
        "meta": meta,
    })
    .to_string();

    let mut delivered = 0;
    {
        let clients = hub.clients.lock().unwrap();
        for (name, tx) in clients.iter() {
            let is_target = if to == "all" { *name != from } else { *name == to };
            if is_target && tx.send(out.clone()).is_ok() {
                delivered += 1;
            }
        }
    }
    Json(json!({ "delivered": delivered }))
}
