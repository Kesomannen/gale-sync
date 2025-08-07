use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    hash::Hash,
    sync::{Arc, Mutex},
};

use anyhow::{bail, Context};
use axum::extract::ws::{self, WebSocket};
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::{
    profile::{ProfileId, ProfileMetadata},
    RedisConn,
};

const PROFILE_UPDATE: &str = "profile-update";
const PROFILE_DELETE: &str = "profile-delete";

type ListenerMap = HashMap<ProfileId, HashSet<Listener>>;

#[derive(Clone)]
pub struct State {
    listeners: Arc<Mutex<ListenerMap>>,
}

impl State {
    pub fn new(redis: mpsc::UnboundedReceiver<redis::PushInfo>) -> Self {
        let state = Self {
            listeners: Default::default(),
        };

        tokio::spawn(handle_redis(state.clone(), redis));

        state
    }

    pub async fn notify_profile_updated(
        &self,
        redis: &mut RedisConn,
        metadata: &ProfileMetadata,
    ) -> anyhow::Result<()> {
        self.notify_redis(
            redis,
            format!("{PROFILE_UPDATE}:{}", metadata.short_id),
            metadata,
        )
        .await
    }

    pub async fn notify_profile_deleted(
        &self,
        redis: &mut RedisConn,
        id: &ProfileId,
    ) -> anyhow::Result<()> {
        self.notify_redis(redis, format!("{PROFILE_DELETE}:{id}",), id)
            .await
    }

    async fn notify_redis<T: Serialize>(
        &self,
        redis: &mut RedisConn,
        channel: String,
        payload: &T,
    ) -> anyhow::Result<()> {
        let json = serde_json::to_string(payload).context("failed to serialize event payload")?;

        redis::cmd("PUBLISH")
            .arg(&[channel, json])
            .query_async::<()>(redis)
            .await?;

        Ok(())
    }

    fn notify_local(listeners: &mut ListenerMap, profile_id: &ProfileId, message: ServerMessage) {
        let mut count = 0;

        if let Some(set) = listeners.get(profile_id) {
            for listener in set {
                if listener.tx.send(message.clone()).is_err() {
                    warn!(
                        "failed to send profile changed message to listener {}",
                        listener.uuid
                    );
                } else {
                    count += 1;
                }
            }
        }

        info!("notified {count} listeners");
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
    ProfileUpdated { metadata: ProfileMetadata },
    ProfileDeleted { id: ProfileId },
    Error { message: Cow<'static, str> },
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

async fn handle_redis(state: State, mut redis: mpsc::UnboundedReceiver<redis::PushInfo>) {
    while let Some(msg) = redis.recv().await {
        if let Err(err) = handle_redis_message(&state, msg).await {
            error!("failed to handle redis message: {err}");
        }
    }
}

async fn handle_redis_message(state: &State, msg: redis::PushInfo) -> anyhow::Result<()> {
    debug!("{msg:?}");

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

            State::notify_local(
                &mut listeners,
                &profile_id,
                ServerMessage::ProfileUpdated { metadata },
            );
        }
        PROFILE_DELETE => {
            let id: ProfileId = serde_json::from_str(&payload)?;

            State::notify_local(
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

pub struct PushSender {
    tx: mpsc::UnboundedSender<redis::PushInfo>,
}

impl PushSender {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<redis::PushInfo>) {
        let (tx, rx) = mpsc::unbounded_channel();

        (Self { tx }, rx)
    }
}

impl redis::aio::AsyncPushSender for PushSender {
    fn send(&self, info: redis::PushInfo) -> Result<(), redis::aio::SendError> {
        self.tx.send(info).map_err(|_| redis::aio::SendError)
    }
}
