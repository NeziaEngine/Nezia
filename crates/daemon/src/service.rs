//! `PreviewDaemon` gRPC service の実装。
//!
//! 各 RPC を `EngineHandle` への要求に変換し、エンジンスレッドからの応答を待って
//! gRPC レスポンスに詰め替える。失敗は `tonic::Status` にマップする。

use tonic::{Request, Response, Status};

use crate::engine::{EngineError, EngineHandle};
use crate::proto::v1::preview_daemon_server::PreviewDaemon;
use crate::proto::v1::{
    BufferId as ProtoBufferId, LoadBufferRequest, LoadBufferResponse, PingRequest, PingResponse,
    PlayRequest, PlayResponse, SourceHandle as ProtoSourceHandle, StopAllRequest, StopAllResponse,
    StopRequest, StopResponse,
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

/// エンジンスレッド消失は gRPC 上「サービス利用不可」として扱う。
fn unavailable(_: EngineError) -> Status {
    Status::unavailable("engine thread is no longer running")
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
            Err(err) => Err(unavailable(err)),
        }
    }

    async fn play(&self, request: Request<PlayRequest>) -> Result<Response<PlayResponse>, Status> {
        let req = request.into_inner();
        let buffer = req
            .buffer
            .as_ref()
            .map(from_proto_buffer)
            .ok_or_else(|| Status::invalid_argument("buffer is required"))?;

        match self
            .engine
            .play(buffer, req.volume, req.pitch, req.looping)
            .await
            .map_err(unavailable)?
        {
            Some(id) => Ok(Response::new(PlayResponse {
                source: Some(to_proto_source(id)),
            })),
            None => Err(Status::resource_exhausted(
                "could not spawn source (invalid buffer or voice capacity reached)",
            )),
        }
    }

    async fn stop(&self, request: Request<StopRequest>) -> Result<Response<StopResponse>, Status> {
        let source = request
            .into_inner()
            .source
            .as_ref()
            .map(from_proto_source)
            .ok_or_else(|| Status::invalid_argument("source is required"))?;
        let accepted = self.engine.stop(source).await.map_err(unavailable)?;
        Ok(Response::new(StopResponse { accepted }))
    }

    async fn stop_all(
        &self,
        _request: Request<StopAllRequest>,
    ) -> Result<Response<StopAllResponse>, Status> {
        let accepted = self.engine.stop_all().await.map_err(unavailable)?;
        Ok(Response::new(StopAllResponse { accepted }))
    }

    async fn ping(&self, _request: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
        Ok(Response::new(PingResponse {
            version: self.version.clone(),
        }))
    }
}
