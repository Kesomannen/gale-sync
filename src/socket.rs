use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    hash::Hash,
    sync::{Arc, Mutex},
};

use anyhow::bail;
use axum::extract::ws::{self, WebSocket};
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::profile::{ProfileId, ProfileMetadata};

type ListenerMap = HashMap<ProfileId, HashSet<Listener>>;

#[derive(Clone)]
pub struct State {
    listeners: Arc<Mutex<ListenerMap>>,
}

impl State {
    pub fn new() -> Self {
        Self {
            listeners: Default::default(),
        }
    }

    pub fn notify_profile_updated(&self, metadata: ProfileMetadata) {
        let mut listeners = self.listeners.lock().unwrap();

        Self::notify(
            &mut listeners,
            &metadata.short_id.clone(),
            ServerMessage::ProfileUpdated { metadata },
        );
    }

    pub fn notify_profile_deleted(&self, id: ProfileId) {
        let mut listeners = self.listeners.lock().unwrap();

        Self::notify(
            &mut listeners,
            &id.clone(),
            ServerMessage::ProfileDeleted { id: id.clone() },
        );

        listeners.remove(&id);
    }

    fn notify(listeners: &mut ListenerMap, profile_id: &ProfileId, message: ServerMessage) {
        if let Some(set) = listeners.get(profile_id) {
            for listener in set {
                if listener.tx.send(message.clone()).is_err() {
                    warn!(
                        "failed to send profile changed message to listener {}",
                        listener.uuid
                    );
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Listener {
    uuid: Uuid,
    tx: mpsc::UnboundedSender<ServerMessage>,
}

impl Listener {
    fn new(tx: mpsc::UnboundedSender<ServerMessage>) -> Self {
        Self {
            uuid: Uuid::new_v4(),
            tx,
        }
    }
}

impl PartialEq for Listener {
    fn eq(&self, other: &Self) -> bool {
        self.uuid == other.uuid
    }
}

impl Eq for Listener {}

impl Hash for Listener {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.uuid.hash(state);
    }
}

#[derive(Debug, Serialize, Clone)]
#[serde(tag = "event", content = "payload", rename_all = "camelCase")]
enum ServerMessage {
    #[serde(rename_all = "camelCase")]
    ProfileUpdated {
        metadata: ProfileMetadata,
    },

    #[serde(rename_all = "camelCase")]
    ProfileDeleted {
        id: ProfileId,
    },

    Error {
        message: Cow<'static, str>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "event", content = "payload", rename_all = "camelCase")]
enum ClientMessage {
    #[serde(rename_all = "camelCase")]
    Subscribe { profile_id: ProfileId },

    #[serde(rename_all = "camelCase")]
    Unsubscribe { profile_id: ProfileId },
}

pub(crate) async fn handle(socket: WebSocket, state: State) {
    let (sender, receiver) = socket.split();
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(write(sender, rx));
    tokio::spawn(read(receiver, Listener::new(tx), state));
}

async fn read(receiver: SplitStream<WebSocket>, listener: Listener, state: State) {
    match read_inner(receiver, &listener, &state).await {
        Ok(close_reason) => info!("stopping socket read task: {close_reason}"),
        Err(err) => warn!("error running socket read task, stopping: {err}"),
    };

    let mut listeners = state.listeners.lock().unwrap();

    for set in listeners.values_mut() {
        set.remove(&listener);
    }
}

async fn read_inner(
    mut receiver: SplitStream<WebSocket>,
    listener: &Listener,
    state: &State,
) -> anyhow::Result<&'static str> {
    while let Some(item) = receiver.next().await {
        let item = item?;

        let text = match item {
            ws::Message::Text(utf8_bytes) => utf8_bytes,
            ws::Message::Close(_) => {
                return Ok("close message received");
            }
            other => {
                warn!("received unexpected message: {other:?}");
                continue;
            }
        };

        let response = match serde_json::from_str::<ClientMessage>(text.as_ref()) {
            Ok(ClientMessage::Subscribe { profile_id }) => {
                let mut listeners = state.listeners.lock().unwrap();

                listeners
                    .entry(profile_id)
                    .or_default()
                    .insert(listener.clone());

                None
            }
            Ok(ClientMessage::Unsubscribe { profile_id }) => {
                let mut listeners = state.listeners.lock().unwrap();

                listeners.entry(profile_id).or_default().remove(&listener);

                None
            }
            Err(err) => {
                let response = ServerMessage::Error {
                    message: format!("Failed to deserialize message: {err}.").into(),
                };

                Some(response)
            }
        };

        if let Some(response) = response {
            if listener.tx.send(response).is_err() {
                bail!("send channel closed");
            }
        }
    }

    Ok("socket closed")
}

async fn write(
    mut sender: SplitSink<WebSocket, ws::Message>,
    mut rx: mpsc::UnboundedReceiver<ServerMessage>,
) {
    while let Some(msg) = rx.recv().await {
        let msg = match serde_json::to_string(&msg) {
            Ok(str) => ws::Message::Text(str.into()),
            Err(err) => {
                error!("stopping socket write task: failed to serialize socket message: {err}");
                continue;
            }
        };

        if let Err(err) = sender.send(msg).await {
            warn!("stopping socket write task: transmit error: {err}");
            return;
        }
    }

    info!("stopping socket write task: channel was closed")
}
