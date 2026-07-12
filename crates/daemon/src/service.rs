//! `PreviewDaemon` gRPC service の実装。
//!
//! 各 RPC を `EngineHandle` への要求に変換し、エンジンスレッドからの応答を待って
//! gRPC レスポンスに詰め替える。失敗は `tonic::Status` にマップする。

use std::pin::Pin;

use tokio::sync::broadcast;
use tokio_stream::Stream;
use tonic::{Request, Response, Status};

use crate::engine::{EngineError, EngineHandle, PlayReply};
use crate::proto::v1::engine_event;
use crate::proto::v1::preview_daemon_server::PreviewDaemon;
use crate::proto::v1::{
    BufferId as ProtoBufferId, BusHandle as ProtoBusHandle, CaptureOverflowEvent, EngineEvent,
    LoadBufferRequest, LoadBufferResponse, LoadMixerRequest, LoadMixerResponse, NamedBus,
    PingRequest, PingResponse, PlayFailedEvent, PlayRequest, PlayResponse,
    SourceHandle as ProtoSourceHandle, SourceStoppedEvent, StopAllRequest, StopAllResponse,
    StopRequest, StopResponse, StreamingUnderrunEvent, SubscribeEventsRequest,
    SubscriberLaggedEvent,
};

pub struct PreviewService {
    engine: EngineHandle,
    version: String,
}

impl PreviewService {
    pub fn new(engine: EngineHandle, version: impl Into<String>) -> Self {
        Self {
            engine,
            version: version.into(),
        }
    }
}

/// `EngineError` を gRPC Status にマップする。
fn map_engine_error(err: EngineError) -> Status {
    match err {
        EngineError::Disconnected => Status::unavailable("engine thread is no longer running"),
        EngineError::Load(msg) => Status::internal(msg),
        EngineError::Invalid(msg) => Status::invalid_argument(msg),
    }
}

fn to_proto_buffer(id: nezia_core::BufferId) -> ProtoBufferId {
    ProtoBufferId {
        index: id.index,
        generation: id.generation,
    }
}

fn from_proto_buffer(id: &ProtoBufferId) -> nezia_core::BufferId {
    nezia_core::BufferId {
        index: id.index,
        generation: id.generation,
    }
}

fn to_proto_source(id: nezia_core::EntityId) -> ProtoSourceHandle {
    ProtoSourceHandle {
        index: id.index,
        generation: id.generation,
    }
}

fn from_proto_source(handle: &ProtoSourceHandle) -> nezia_core::EntityId {
    nezia_core::EntityId {
        index: handle.index,
        generation: handle.generation,
    }
}

/// core のイベントを proto へ変換する。外部ツールに意味のないもの
/// (`SourceFinished` はコールバック token という内部表現のため) は `None`。
fn to_proto_event(ev: nezia_core::Event) -> Option<engine_event::Event> {
    match ev {
        // token はコールバックレジストリの内部 ID なので露出しない。ソースの
        // 終了自体は直後に流れる SourceDespawned (→ SourceStopped) で観測できる。
        nezia_core::Event::SourceFinished { .. } => None,
        nezia_core::Event::PlayFailed { .. } => {
            Some(engine_event::Event::PlayFailed(PlayFailedEvent {}))
        }
        nezia_core::Event::SourceDespawned { id } => {
            Some(engine_event::Event::SourceStopped(SourceStoppedEvent {
                source: Some(to_proto_source(id)),
            }))
        }
        nezia_core::Event::StreamingUnderrun { buffer } => Some(
            engine_event::Event::StreamingUnderrun(StreamingUnderrunEvent {
                buffer: Some(to_proto_buffer(buffer)),
            }),
        ),
        nezia_core::Event::CaptureOverflow { dropped_samples } => Some(
            engine_event::Event::CaptureOverflow(CaptureOverflowEvent { dropped_samples }),
        ),
    }
}

#[tonic::async_trait]
impl PreviewDaemon for PreviewService {
    async fn load_buffer(
        &self,
        request: Request<LoadBufferRequest>,
    ) -> Result<Response<LoadBufferResponse>, Status> {
        let path = request.into_inner().path;
        if path.is_empty() {
            return Err(Status::invalid_argument("path must not be empty"));
        }
        match self.engine.load(path).await {
            Ok(id) => Ok(Response::new(LoadBufferResponse {
                buffer: Some(to_proto_buffer(id)),
            })),
            Err(EngineError::Load(msg)) => Err(Status::internal(msg)),
            Err(err) => Err(map_engine_error(err)),
        }
    }

    async fn play(&self, request: Request<PlayRequest>) -> Result<Response<PlayResponse>, Status> {
        let req = request.into_inner();
        let buffer = req
            .buffer
            .as_ref()
            .map(from_proto_buffer)
            .ok_or_else(|| Status::invalid_argument("buffer is required"))?;
        let bus = (!req.bus.is_empty()).then(|| req.bus.clone());

        match self
            .engine
            .play(buffer, req.volume, req.pitch, req.looping, bus)
            .await
            .map_err(map_engine_error)?
        {
            PlayReply::Source(Some(id)) => Ok(Response::new(PlayResponse {
                source: Some(to_proto_source(id)),
            })),
            PlayReply::Source(None) => Err(Status::resource_exhausted(
                "could not spawn source (invalid buffer or voice capacity reached)",
            )),
            PlayReply::UnknownBus => Err(Status::not_found(format!(
                "bus {:?} not found (load a mixer first)",
                req.bus
            ))),
        }
    }

    async fn load_mixer(
        &self,
        request: Request<LoadMixerRequest>,
    ) -> Result<Response<LoadMixerResponse>, Status> {
        let def = request
            .into_inner()
            .mixer
            .ok_or_else(|| Status::invalid_argument("mixer is required"))?;
        let buses = self.engine.load_mixer(def).await.map_err(map_engine_error)?;
        Ok(Response::new(LoadMixerResponse {
            buses: buses
                .into_iter()
                .map(|(name, id)| NamedBus {
                    name,
                    bus: Some(ProtoBusHandle {
                        index: id.index,
                        generation: id.generation,
                    }),
                })
                .collect(),
        }))
    }

    async fn stop(&self, request: Request<StopRequest>) -> Result<Response<StopResponse>, Status> {
        let source = request
            .into_inner()
            .source
            .as_ref()
            .map(from_proto_source)
            .ok_or_else(|| Status::invalid_argument("source is required"))?;
        let accepted = self.engine.stop(source).await.map_err(map_engine_error)?;
        Ok(Response::new(StopResponse { accepted }))
    }

    async fn stop_all(
        &self,
        _request: Request<StopAllRequest>,
    ) -> Result<Response<StopAllResponse>, Status> {
        let accepted = self.engine.stop_all().await.map_err(map_engine_error)?;
        Ok(Response::new(StopAllResponse { accepted }))
    }

    async fn ping(&self, _request: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
        Ok(Response::new(PingResponse {
            version: self.version.clone(),
        }))
    }

    type SubscribeEventsStream = Pin<Box<dyn Stream<Item = Result<EngineEvent, Status>> + Send>>;

    async fn subscribe_events(
        &self,
        _request: Request<SubscribeEventsRequest>,
    ) -> Result<Response<Self::SubscribeEventsStream>, Status> {
        let mut rx = self.engine.subscribe_events();
        // broadcast → gRPC ストリームへの中継タスク。クライアント切断で
        // mpsc の send が失敗し、タスクは自然に終了する。
        let (tx, out) = tokio::sync::mpsc::channel::<Result<EngineEvent, Status>>(64);
        tokio::spawn(async move {
            loop {
                let item = match rx.recv().await {
                    Ok(ev) => match to_proto_event(ev) {
                        Some(event) => EngineEvent { event: Some(event) },
                        None => continue,
                    },
                    // 購読者が配信に追いつけなかった: 取りこぼし数を通知して継続。
                    Err(broadcast::error::RecvError::Lagged(n)) => EngineEvent {
                        event: Some(engine_event::Event::SubscriberLagged(
                            SubscriberLaggedEvent { events_dropped: n },
                        )),
                    },
                    // エンジンスレッド終了 → ストリームを閉じる。
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                if tx.send(Ok(item)).await.is_err() {
                    break; // クライアント切断。
                }
            }
        });
        Ok(Response::new(Box::pin(
            tokio_stream::wrappers::ReceiverStream::new(out),
        )))
    }
}
