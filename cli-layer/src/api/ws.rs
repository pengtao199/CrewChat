//! WebSocket attach/replay/event bridge, matching the AgentOS event-stream shape.

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};

use crate::api::http::AppState;

pub async fn serve(state: AppState, socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();
    let mut events = state.sessions.subscribe();
    let mut connection_id: Option<String> = None;

    if sender
        .send(Message::Text(
            json!({"channel": "__ready__"}).to_string().into(),
        ))
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            incoming = receiver.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(value) = serde_json::from_str::<Value>(text.as_str()) else {
                            continue;
                        };
                        match value.get("action").and_then(Value::as_str) {
                            Some("attach") => {
                                connection_id = value
                                    .get("connection_id")
                                    .and_then(Value::as_str)
                                    .map(str::to_string);
                                let since = value
                                    .get("since_seq")
                                    .and_then(Value::as_u64)
                                    .unwrap_or(0);
                                if let Some(conn) = &connection_id {
                                    let replay = state
                                        .sessions
                                        .events_since(conn, since)
                                        .unwrap_or_default();
                                    let frame = json!({"type": "replay", "events": replay});
                                    if sender
                                        .send(Message::Text(frame.to_string().into()))
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                let _ = sender
                                    .send(Message::Text(
                                        json!({"type": "attached"}).to_string().into(),
                                    ))
                                    .await;
                            }
                            Some("ping") => {
                                let _ = sender
                                    .send(Message::Text(
                                        json!({"type": "pong"}).to_string().into(),
                                    ))
                                    .await;
                            }
                            Some("detach") => connection_id = None,
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        let _ = sender.send(Message::Pong(payload)).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
            event = events.recv() => {
                match event {
                    Some(event) => {
                        if let Some(conn) = &connection_id {
                            if event.connection_id != *conn {
                                continue;
                            }
                        }
                        let frame = json!({"type": "event", "envelope": event});
                        if sender
                            .send(Message::Text(frame.to_string().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }
}
