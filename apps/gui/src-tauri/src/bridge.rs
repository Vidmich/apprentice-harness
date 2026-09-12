//! The generic RPC bridge: `rpc_call` passes any method through untouched,
//! `rpc_stream` does the same for streaming methods and re-emits every
//! event of the subscription as the Tauri event `rpc:event:<channel>`,
//! where `channel` is a name the frontend picked before calling (so it
//! can listen before the first event exists). Keeping the bridge untyped
//! means a new daemon method needs no Rust change here — only the
//! frontend's `api.ts`.

use apprentice_api::events::{AgentStatus, Event, EventNotification};
use apprentice_api::jsonrpc::{RpcError, codes};
use apprentice_client::{ClientError, EventStream};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter};
use tracing::{debug, warn};

use crate::daemon::DaemonState;

/// Tauri event name for the events of one channel.
pub fn event_name(channel: &str) -> String {
    format!("rpc:event:{channel}")
}

/// Turns a client-side failure into the same error shape the daemon uses,
/// so the frontend handles both alike (`data.kind` tells them apart).
pub fn client_error(e: ClientError) -> RpcError {
    match e {
        ClientError::Rpc(e) => e,
        ClientError::Closed => RpcError::new(
            codes::INTERNAL_ERROR,
            "daemon_unavailable",
            "connection to daemon closed",
        ),
        ClientError::Timeout(t) => RpcError::new(
            codes::INTERNAL_ERROR,
            "timeout",
            format!("request timed out after {t:?}"),
        ),
        ClientError::Io(e) => RpcError::new(
            codes::INTERNAL_ERROR,
            "transport",
            format!("transport error: {e}"),
        ),
        ClientError::Protocol(m) => RpcError::new(codes::INTERNAL_ERROR, "protocol", m),
    }
}

/// Any daemon method, untyped.
#[tauri::command]
pub async fn rpc_call(
    state: tauri::State<'_, DaemonState>,
    method: String,
    params: Value,
) -> Result<Value, RpcError> {
    let client = state.client()?;
    client.call_raw(&method, params).await.map_err(client_error)
}

/// What `rpc_stream` returns: the method's own result plus the daemon
/// subscription the events on the channel belong to.
#[derive(Debug, Clone, Serialize)]
pub struct StreamStarted {
    pub subscription: String,
    pub result: Value,
}

/// A streaming method (`agent.run`, `agent.subscribe`): calls it, then
/// forwards the subscription's events to `rpc:event:<channel>` until the
/// terminal one.
#[tauri::command]
pub async fn rpc_stream(
    app: AppHandle,
    state: tauri::State<'_, DaemonState>,
    method: String,
    params: Value,
    channel: String,
) -> Result<StreamStarted, RpcError> {
    if channel.is_empty() {
        return Err(RpcError::invalid_params("channel must not be empty"));
    }
    let client = state.client()?;
    let result = client
        .call_raw(&method, params)
        .await
        .map_err(client_error)?;
    let Some(subscription) = result.get("subscription").and_then(Value::as_str) else {
        return Err(RpcError::new(
            codes::INTERNAL_ERROR,
            "protocol",
            format!("{method} is not a streaming method: its result has no `subscription`"),
        ));
    };
    let subscription = subscription.to_owned();
    // `agent.run` names the agent; `agent.subscribe` only the subscription
    // (which is the agent id too).
    let agent_id = result
        .get("agent_id")
        .and_then(Value::as_str)
        .map_or_else(|| subscription.clone(), str::to_owned);
    let stream = client.subscribe(&subscription).await;
    let name = event_name(&channel);
    let sub = subscription.clone();
    tauri::async_runtime::spawn(async move {
        forward(stream, &sub, &agent_id, |ev| {
            if let Err(e) = app.emit(&name, &ev) {
                warn!(error = %e, subscription = sub, "cannot emit daemon event");
            }
        })
        .await;
    });
    Ok(StreamStarted {
        subscription,
        result,
    })
}

/// Delivers every event of `stream` to `emit`, in order, and stops after
/// the terminal one. A stream that ends without it (the connection went
/// away) gets a synthetic `agent.finished{error}` so the frontend never
/// waits on a run that cannot finish.
pub async fn forward(
    mut stream: EventStream,
    subscription: &str,
    agent_id: &str,
    emit: impl Fn(EventNotification),
) {
    let mut seq = 0;
    while let Some(ev) = stream.next().await {
        seq = ev.seq;
        let terminal = ev.event.is_terminal();
        emit(ev);
        if terminal {
            debug!(subscription, "subscription finished");
            return;
        }
    }
    debug!(subscription, "subscription ended without a terminal event");
    emit(EventNotification {
        subscription: subscription.to_owned(),
        seq: seq + 1,
        event: Event::AgentFinished {
            agent_id: agent_id.to_owned(),
            status: AgentStatus::Error,
            error: Some(RpcError::new(
                codes::INTERNAL_ERROR,
                "daemon_unavailable",
                "connection to daemon closed",
            )),
            truncated: false,
        },
    });
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use apprentice_api::codec::write_message;
    use apprentice_api::events::EVENT_METHOD;
    use apprentice_api::jsonrpc::Message;
    use apprentice_client::{ClientOptions, DaemonClient};
    use tokio::io::AsyncWriteExt as _;

    use super::*;

    fn notification(subscription: &str, seq: u64, event: Event) -> Message {
        Message::notification(
            EVENT_METHOD,
            Some(
                serde_json::to_value(EventNotification {
                    subscription: subscription.into(),
                    seq,
                    event,
                })
                .unwrap(),
            ),
        )
    }

    fn delta(agent: &str, text: &str) -> Event {
        Event::AgentTextDelta {
            agent_id: agent.into(),
            text: text.into(),
        }
    }

    fn finished(agent: &str, status: AgentStatus) -> Event {
        Event::AgentFinished {
            agent_id: agent.into(),
            status,
            error: None,
            truncated: false,
        }
    }

    /// A client whose "daemon" is the returned writer.
    fn client() -> (DaemonClient, tokio::io::WriteHalf<tokio::io::DuplexStream>) {
        let (daemon_side, client_side) = tokio::io::duplex(1 << 16);
        let (_dr, dw) = tokio::io::split(daemon_side);
        let (cr, cw) = tokio::io::split(client_side);
        (
            DaemonClient::from_streams(cr, cw, ClientOptions::default()),
            dw,
        )
    }

    type Seen = Arc<Mutex<Vec<EventNotification>>>;

    fn collector() -> (Seen, impl Fn(EventNotification)) {
        let seen: Seen = Arc::default();
        let sink = seen.clone();
        (seen, move |ev| sink.lock().unwrap().push(ev))
    }

    #[tokio::test]
    async fn events_of_two_subscriptions_are_routed_apart_and_stop_at_finished() {
        let (client, mut daemon) = client();
        let interleaved = [
            notification("a", 1, delta("a", "A1")),
            notification("b", 1, delta("b", "B1")),
            notification("a", 2, delta("a", "A2")),
            notification("a", 3, finished("a", AgentStatus::Ok)),
            notification("b", 2, delta("b", "B2")),
            notification("b", 3, finished("b", AgentStatus::Cancelled)),
        ];
        for m in &interleaved {
            write_message(&mut daemon, m).await.unwrap();
        }
        daemon.flush().await.unwrap();

        let (seen_a, emit_a) = collector();
        forward(client.subscribe("a").await, "a", "a", emit_a).await;
        let a = std::mem::take(&mut *seen_a.lock().unwrap());
        assert_eq!(a.len(), 3, "{a:?}");
        assert!(a.iter().all(|ev| ev.subscription == "a"));
        assert_eq!(a.iter().map(|ev| ev.seq).collect::<Vec<_>>(), [1, 2, 3]);
        assert!(matches!(
            a[2].event,
            Event::AgentFinished {
                status: AgentStatus::Ok,
                ..
            }
        ));

        // `b`'s events were held back, not lost or delivered to `a`.
        let (seen_b, emit_b) = collector();
        forward(client.subscribe("b").await, "b", "b", emit_b).await;
        let b = std::mem::take(&mut *seen_b.lock().unwrap());
        assert_eq!(b.len(), 3, "{b:?}");
        assert!(b.iter().all(|ev| ev.subscription == "b"));
        assert!(matches!(
            b[2].event,
            Event::AgentFinished {
                status: AgentStatus::Cancelled,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn a_stream_cut_short_ends_with_a_synthetic_error_finish() {
        let (client, mut daemon) = client();
        write_message(&mut daemon, &notification("a", 1, delta("a", "partial")))
            .await
            .unwrap();
        daemon.flush().await.unwrap();
        let stream = client.subscribe("a").await;
        // Give the reader time to route the delta, then hang up.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        drop(daemon);

        let (seen, emit) = collector();
        forward(stream, "a", "agent-1", emit).await;
        let seen = std::mem::take(&mut *seen.lock().unwrap());
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert_eq!(seen[0].seq, 1);
        assert_eq!(seen[1].seq, 2);
        match &seen[1].event {
            Event::AgentFinished {
                agent_id,
                status: AgentStatus::Error,
                error: Some(e),
                ..
            } => {
                assert_eq!(agent_id, "agent-1");
                assert_eq!(e.kind(), Some("daemon_unavailable"));
            }
            other => panic!("expected a synthetic finish, got {other:?}"),
        }
    }

    #[test]
    fn client_errors_carry_a_kind() {
        assert_eq!(
            client_error(ClientError::Closed).kind(),
            Some("daemon_unavailable")
        );
        assert_eq!(
            client_error(ClientError::Timeout(std::time::Duration::from_secs(1))).kind(),
            Some("timeout")
        );
        let rpc = RpcError::not_found("x");
        assert_eq!(client_error(ClientError::Rpc(rpc.clone())), rpc);
        assert_eq!(event_name("agent-1"), "rpc:event:agent-1");
    }
}
