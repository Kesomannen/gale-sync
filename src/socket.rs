use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    hash::Hash,
    sync::{Arc, Mutex},
};

use anyhow::bail;
use axum::{
    body::Bytes,
    extract::ws::{self, WebSocket},
};
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};
use uuid::Uuid;

use crate::{
    profile::{self, ProfileId, ProfileMetadata},
    AppState, Redis,
};

const PROFILE_UPDATE: &str = "profile-update";
const PROFILE_DELETE: &str = "profile-delete";

type ListenerMap = HashMap<ProfileId, HashSet<Listener>>;

#[derive(Clone)]
pub struct SocketState {
    listeners: Arc<Mutex<ListenerMap>>,
}

impl SocketState {
    pub fn new(redis: mpsc::UnboundedReceiver<redis::PushInfo>) -> Self {
        let state = Self {
            listeners: Default::default(),
        };

        tokio::spawn(handle_redis(state.clone(), redis));

        state
    }

    pub fn notify_profile_updated(&self, redis: Redis, metadata: &ProfileMetadata) {
        self.notify_redis(
            redis,
            format!("{PROFILE_UPDATE}:{}", metadata.short_id),
            metadata,
        )
    }

    pub fn notify_profile_deleted(&self, redis: Redis, id: &ProfileId) {
        self.notify_redis(redis, format!("{PROFILE_DELETE}:{id}",), id)
    }

    fn notify_redis<T: Serialize>(&self, mut redis: Redis, channel: String, payload: &T) {
        let json = match serde_json::to_string(payload) {
            Ok(str) => str,
            Err(err) => {
                error!(?err, "failed to serailize redis event payload");
                return;
            }
        };

        tokio::spawn(async move {
            let result = redis::cmd("PUBLISH")
                .arg(&[channel, json])
                .query_async::<()>(&mut redis)
                .await;

            if let Err(err) = result {
                error!(?err, "failed to publish redis message");
            }
        });
    }

    fn notify_local(listeners: &mut ListenerMap, profile_id: &ProfileId, message: ServerMessage) {
        let Some(set) = listeners.get(profile_id) else {
            debug!("no listeners for profile {profile_id}");
            return;
        };

        for listener in set {
            if listener.tx.try_send(message.clone()).is_err() {
                warn!(
                    %profile_id,
                    listener_id = ?listener.uuid,
                    "failed to send message to listener",
                );
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Listener {
    uuid: Uuid,
    tx: mpsc::Sender<ServerMessage>,
}

impl Listener {
    fn new(tx: mpsc::Sender<ServerMessage>) -> Self {
        Self {
            uuid: Uuid::new_v4(),
            tx,
        }
    }

    fn try_send(&self, message: ServerMessage) -> anyhow::Result<()> {
        match self.tx.try_send(message) {
            Ok(_) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => bail!("listener channel closed"),
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!(
                    listener_id = ?self.uuid,
                    "failed to send message to listener: channel full, dropping message",
                );
                Ok(())
            }
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
#[serde(
    tag = "event",
    content = "payload",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum ServerMessage {
    ProfileUpdated {
        metadata: ProfileMetadata,
    },
    ProfileDeleted {
        id: ProfileId,
    },
    ProfileNotFound {
        id: ProfileId,
    },
    Error {
        message: Cow<'static, str>,
    },
    #[serde(skip_serializing)]
    Pong(Bytes),
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "event",
    content = "payload",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum ClientMessage {
    Subscribe { profile_id: ProfileId },
    Unsubscribe { profile_id: ProfileId },
}

pub(crate) fn handle_connection(socket: WebSocket, state: AppState) {
    let (sender, receiver) = socket.split();
    let (tx, rx) = mpsc::channel(16);

    tokio::spawn(write(sender, rx));
    tokio::spawn(read(receiver, Listener::new(tx), state));
}

async fn read(receiver: SplitStream<WebSocket>, listener: Listener, state: AppState) {
    match read_inner(receiver, &listener, &state).await {
        Ok(_) => (),
        Err(err) => warn!("error running socket read task, stopping: {err}"),
    };

    let mut listeners = state.sockets.listeners.lock().unwrap();

    for set in listeners.values_mut() {
        set.remove(&listener);
    }
}

async fn read_inner(
    mut receiver: SplitStream<WebSocket>,
    listener: &Listener,
    state: &AppState,
) -> anyhow::Result<()> {
    while let Some(item) = receiver.next().await {
        let item = item?;

        match item {
            ws::Message::Text(text) => {
                handle_text_message(listener, state, text).await?;
            }
            ws::Message::Ping(ping) => {
                handle_ping(listener, ping).await?;
            }
            ws::Message::Close(close) => {
                debug!(?close, "socket closed");
                return Ok(());
            }
            msg => {
                debug!(?msg, "received unexpected message");
                continue;
            }
        };
    }

    Ok(())
}

async fn handle_text_message(
    listener: &Listener,
    state: &AppState,
    text: ws::Utf8Bytes,
) -> Result<(), anyhow::Error> {
    let response = match serde_json::from_str::<ClientMessage>(text.as_ref()) {
        Ok(msg) => handle_client_message(listener, state, msg).await?,
        Err(err) => {
            let response = ServerMessage::Error {
                message: format!("Failed to deserialize message: {err}.").into(),
            };

            Some(response)
        }
    };

    if let Some(response) = response {
        match listener.tx.try_send(response) {
            Ok(_) => (),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                bail!("listener channel closed");
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!("failed to send response message to socket: channel full");
            }
        }
    }

    Ok(())
}

async fn handle_client_message(
    listener: &Listener,
    state: &AppState,
    msg: ClientMessage,
) -> anyhow::Result<Option<ServerMessage>> {
    match msg {
        ClientMessage::Subscribe { profile_id } => {
            if profile::exists(state, &profile_id).await? {
                let mut listeners = state.sockets.listeners.lock().unwrap();

                listeners
                    .entry(profile_id)
                    .or_default()
                    .insert(listener.clone());

                Ok(None)
            } else {
                Ok(Some(ServerMessage::ProfileNotFound { id: profile_id }))
            }
        }
        ClientMessage::Unsubscribe { profile_id } => {
            let mut listeners = state.sockets.listeners.lock().unwrap();

            listeners.entry(profile_id).or_default().remove(listener);

            Ok(None)
        }
    }
}

async fn handle_ping(listener: &Listener, ping: Bytes) -> anyhow::Result<()> {
    debug!("received ping, sending pong");

    listener.try_send(ServerMessage::Pong(ping))?;

    Ok(())
}

async fn write(
    mut sender: SplitSink<WebSocket, ws::Message>,
    mut rx: mpsc::Receiver<ServerMessage>,
) {
    while let Some(msg) = rx.recv().await {
        let msg = match msg {
            ServerMessage::Pong(bytes) => ws::Message::Pong(bytes),
            _ => match serde_json::to_string(&msg) {
                Ok(str) => ws::Message::Text(str.into()),
                Err(err) => {
                    error!(?err, "failed to serialize message, dropping message");
                    continue;
                }
            },
        };

        if let Err(err) = sender.send(msg).await {
            error!(?err, "transmit error: stopping socket write task");
            return;
        }
    }

    debug!("channel was closed: stopping socket write task");
}

async fn handle_redis(state: SocketState, mut redis: mpsc::UnboundedReceiver<redis::PushInfo>) {
    while let Some(msg) = redis.recv().await {
        if let Err(err) = handle_redis_message(&state, msg).await {
            error!("failed to handle redis message: {err}");
        }
    }
}

async fn handle_redis_message(state: &SocketState, msg: redis::PushInfo) -> anyhow::Result<()> {
    if msg.kind != redis::PushKind::PMessage {
        return Ok(());
    }

    let mut values = msg.data.into_iter();

    // skip the pattern
    _ = values.next();

    let (event_name, profile_id) = match values.next() {
        Some(redis::Value::BulkString(bytes)) => {
            let str = String::try_from(bytes)?;
            let (event_name, profile_id) = str.split_once(":").expect("no colon in channel name");
            let profile_id: ProfileId = profile_id.to_string().try_into()?;

            (event_name.to_string(), profile_id)
        }
        value => bail!("expected channel name, got {value:?}"),
    };

    let payload = match values.next() {
        Some(redis::Value::BulkString(bytes)) => String::try_from(bytes)?,
        value => bail!("expected event payload, got {value:?}"),
    };

    let mut listeners = state.listeners.lock().unwrap();

    match event_name.as_str() {
        PROFILE_UPDATE => {
            let metadata: ProfileMetadata = serde_json::from_str(&payload)?;

            SocketState::notify_local(
                &mut listeners,
                &profile_id,
                ServerMessage::ProfileUpdated { metadata },
            );
        }
        PROFILE_DELETE => {
            let id: ProfileId = serde_json::from_str(&payload)?;

            SocketState::notify_local(
                &mut listeners,
                &profile_id,
                ServerMessage::ProfileDeleted { id },
            );

            listeners.remove(&profile_id);
        }
        name => bail!("unknown event: {name}"),
    }

    Ok(())
}
