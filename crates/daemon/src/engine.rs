//! SoundEngine を所有する専用スレッドと、そこへ要求を送る `EngineHandle`。
//!
//! `SoundEngine` は cpal の `Stream` を保持するため `!Send` であり、tonic の
//! マルチスレッド runtime をまたいで共有できない。そこで **1 本の専用スレッドが
//! SoundEngine を排他所有**し、gRPC ハンドラは crossbeam channel 経由で要求を送る。
//! 応答は tokio の oneshot で非同期に受け取る。
//!
//! これは daemon CONCEPT.md「ステートフル長寿命 SoundEngine」を、スレッド境界の
//! 制約 (リアルタイムオーディオ + !Send) と両立させる最小構成である。

use std::io;
use std::time::Duration;

use crossbeam_channel::{RecvTimeoutError, Sender, unbounded};
use nezia_core::{BufferId, EntityId, SoundEngine, SpawnSpatialInit};
use tokio::sync::oneshot;

/// preview ボイスの発音優先度。単発試聴なので中庸の固定値で十分。
const PREVIEW_PRIORITY: u8 = 128;

/// SoundEngine スレッドが要求を取りこぼさず処理しつつ、合間に `poll_events()` を
/// 回すためのポーリング間隔。
const POLL_INTERVAL: Duration = Duration::from_millis(16);

/// エンジンスレッドへの要求。応答は同梱した oneshot で返す。
enum EngineRequest {
    Load {
        path: String,
        reply: oneshot::Sender<Result<BufferId, String>>,
    },
    Play {
        buffer: BufferId,
        volume: f32,
        pitch: f32,
        looping: bool,
        reply: oneshot::Sender<Option<EntityId>>,
    },
    Stop {
        source: EntityId,
        reply: oneshot::Sender<bool>,
    },
    StopAll {
        reply: oneshot::Sender<bool>,
    },
}

/// `EngineHandle` 経由の操作で起こりうる失敗。
#[derive(Debug)]
pub enum EngineError {
    /// エンジンスレッドが消失した (init 失敗後の異常終了など)。
    Disconnected,
    /// ロード失敗 (ファイル不在 / デコード不能など)。core からのメッセージを保持する。
    Load(String),
}

/// gRPC ハンドラが保持する、エンジンスレッドへの送信ハンドル。
///
/// `Sender` は Send + Sync + Clone なので tonic service (`&self` 共有) から安全に使える。
#[derive(Clone)]
pub struct EngineHandle {
    tx: Sender<EngineRequest>,
}

impl EngineHandle {
    /// オーディオファイルをロードする。
    pub async fn load(&self, path: String) -> Result<BufferId, EngineError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(EngineRequest::Load { path, reply })
            .map_err(|_| EngineError::Disconnected)?;
        match rx.await {
            Ok(Ok(id)) => Ok(id),
            Ok(Err(msg)) => Err(EngineError::Load(msg)),
            Err(_) => Err(EngineError::Disconnected),
        }
    }

    /// マスターバスにボイスを再生する。`None` は spawn 失敗 (無効バッファ / 容量上限)。
    pub async fn play(
        &self,
        buffer: BufferId,
        volume: f32,
        pitch: f32,
        looping: bool,
    ) -> Result<Option<EntityId>, EngineError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(EngineRequest::Play {
                buffer,
                volume,
                pitch,
                looping,
                reply,
            })
            .map_err(|_| EngineError::Disconnected)?;
        rx.await.map_err(|_| EngineError::Disconnected)
    }

    /// 指定ハンドルのボイスを停止する。戻り値はコマンド受理可否。
    pub async fn stop(&self, source: EntityId) -> Result<bool, EngineError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(EngineRequest::Stop { source, reply })
            .map_err(|_| EngineError::Disconnected)?;
        rx.await.map_err(|_| EngineError::Disconnected)
    }

    /// すべてのボイスを停止する。
    pub async fn stop_all(&self) -> Result<bool, EngineError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(EngineRequest::StopAll { reply })
            .map_err(|_| EngineError::Disconnected)?;
        rx.await.map_err(|_| EngineError::Disconnected)
    }
}

/// エンジンスレッドを起動し、初期化完了を待ってから `EngineHandle` を返す。
///
/// `SoundEngine::new()` (cpal デバイス確保) はスレッド内で行う。初期化失敗は
/// `io::Error` として呼出側へ伝える。
pub fn spawn() -> io::Result<EngineHandle> {
    let (tx, rx) = unbounded::<EngineRequest>();
    // 初期化結果をメインスレッドへ返すための一回限りチャネル。
    let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    std::thread::Builder::new()
        .name("nezia-engine".into())
        .spawn(move || {
            let mut engine = match SoundEngine::new() {
                Ok(engine) => {
                    let _ = init_tx.send(Ok(()));
                    engine
                }
                Err(err) => {
                    let _ = init_tx.send(Err(err.to_string()));
                    return;
                }
            };
            let master = engine.master_bus();
            run_loop(&mut engine, master, &rx);
        })?;

    match init_rx.recv() {
        Ok(Ok(())) => Ok(EngineHandle { tx }),
        Ok(Err(msg)) => Err(io::Error::other(msg)),
        Err(_) => Err(io::Error::other("engine thread exited during init")),
    }
}

/// エンジンスレッドの本体ループ。要求処理と `poll_events()` を交互に回す。
fn run_loop(
    engine: &mut SoundEngine,
    master: EntityId,
    rx: &crossbeam_channel::Receiver<EngineRequest>,
) {
    loop {
        match rx.recv_timeout(POLL_INTERVAL) {
            Ok(req) => {
                handle(engine, master, req);
                // 終了したソースのスロット回収 / コールバック処理。
                engine.poll_events();
            }
            Err(RecvTimeoutError::Timeout) => engine.poll_events(),
            // 全 Sender が drop された (daemon 終了) → ループを抜けてエンジンを破棄。
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn handle(engine: &mut SoundEngine, master: EntityId, req: EngineRequest) {
    match req {
        EngineRequest::Load { path, reply } => {
            let result = engine.load(&path).map_err(|e| e.to_string());
            let _ = reply.send(result);
        }
        EngineRequest::Play {
            buffer,
            volume,
            pitch,
            looping,
            reply,
        } => {
            let handle = engine.play_with_handle(
                buffer,
                volume,
                pitch,
                master,
                looping,
                PREVIEW_PRIORITY,
                SpawnSpatialInit::NONE,
            );
            let _ = reply.send(handle);
        }
        EngineRequest::Stop { source, reply } => {
            let _ = reply.send(engine.stop_source(source));
        }
        EngineRequest::StopAll { reply } => {
            let _ = reply.send(engine.stop_all());
        }
    }
}
