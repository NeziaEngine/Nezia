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
use nezia_core::{BufferId, EntityId, Event, SoundEngine, SpawnSpatialInit};
use tokio::sync::{broadcast, oneshot};

use crate::mixer::{self, MixerState};
use crate::proto::v1::{ClipParams, MixerDef};

/// preview ボイスの発音優先度。単発試聴なので中庸の固定値で十分。
const PREVIEW_PRIORITY: u8 = 128;

/// SoundEngine スレッドが要求を取りこぼさず処理しつつ、合間に `poll_events()` を
/// 回すためのポーリング間隔。
const POLL_INTERVAL: Duration = Duration::from_millis(16);

/// イベント配信 broadcast チャネルの容量。preview 用途のイベント頻度
/// (ソース停止・エラー通知) に対して十分な余裕を持たせる。溢れた場合、
/// 遅い購読者は古いイベントを取りこぼす (Lagged) が配信自体は止まらない。
const EVENT_CHANNEL_CAPACITY: usize = 256;

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
        /// 出力先バスの論理名。`None` = Master 直結。
        bus: Option<String>,
        /// Clip-centric パラメータ (優先度 / 3D / エフェクト / Send)。
        clip: Option<ClipParams>,
        reply: oneshot::Sender<PlayReply>,
    },
    Stop {
        source: EntityId,
        reply: oneshot::Sender<bool>,
    },
    StopAll {
        reply: oneshot::Sender<bool>,
    },
    LoadMixer {
        def: MixerDef,
        reply: oneshot::Sender<Result<Vec<(String, EntityId)>, String>>,
    },
}

/// Play の結果。バス名解決の失敗を spawn 失敗と区別する。
pub enum PlayReply {
    /// spawn 結果 (`None` = 無効バッファ / ボイス上限)。
    Source(Option<EntityId>),
    /// 指定されたバス名が現在のミキサーに存在しない。
    UnknownBus,
    /// ClipParams の適用に失敗した (不正なエフェクト種別 / 未知の send 先など)。
    /// spawn 済みソースは巻き戻し済み。
    ClipInvalid(String),
}

/// `EngineHandle` 経由の操作で起こりうる失敗。
#[derive(Debug)]
pub enum EngineError {
    /// エンジンスレッドが消失した (init 失敗後の異常終了など)。
    Disconnected,
    /// ロード失敗 (ファイル不在 / デコード不能など)。core からのメッセージを保持する。
    Load(String),
    /// リクエスト内容の検証エラー (LoadMixer の不正構成など)。
    Invalid(String),
}

/// gRPC ハンドラが保持する、エンジンスレッドへの送信ハンドル。
///
/// `Sender` は Send + Sync + Clone なので tonic service (`&self` 共有) から安全に使える。
#[derive(Clone)]
pub struct EngineHandle {
    tx: Sender<EngineRequest>,
    events: broadcast::Sender<Event>,
}

impl EngineHandle {
    /// エンジンイベントの購読を開始する。購読開始以降のイベントのみが届く。
    pub fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

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

    /// ボイスを再生する。`bus` はミキサーの論理名 (`None` = Master 直結)。
    pub async fn play(
        &self,
        buffer: BufferId,
        volume: f32,
        pitch: f32,
        looping: bool,
        bus: Option<String>,
        clip: Option<ClipParams>,
    ) -> Result<PlayReply, EngineError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(EngineRequest::Play {
                buffer,
                volume,
                pitch,
                looping,
                bus,
                clip,
                reply,
            })
            .map_err(|_| EngineError::Disconnected)?;
        rx.await.map_err(|_| EngineError::Disconnected)
    }

    /// ミキサー構成を一括ロードする。成功時は (論理名, ハンドル) の生成順リスト。
    /// 検証エラーは `EngineError::Invalid` で返る。
    pub async fn load_mixer(
        &self,
        def: MixerDef,
    ) -> Result<Vec<(String, EntityId)>, EngineError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(EngineRequest::LoadMixer { def, reply })
            .map_err(|_| EngineError::Disconnected)?;
        match rx.await {
            Ok(Ok(buses)) => Ok(buses),
            Ok(Err(msg)) => Err(EngineError::Invalid(msg)),
            Err(_) => Err(EngineError::Disconnected),
        }
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
    let (events_tx, _) = broadcast::channel::<Event>(EVENT_CHANNEL_CAPACITY);
    // 初期化結果をメインスレッドへ返すための一回限りチャネル。
    let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    let sink_tx = events_tx.clone();
    // クリップエフェクト回収用の購読。spawn 時点で subscribe しておくことで
    // 最初の Play より前のイベントも取りこぼさない。
    let reaper_rx = events_tx.subscribe();
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
            // poll_events が drain した全イベントを broadcast へ横流しする。
            // send は購読者ゼロだと Err を返すが、それは正常 (誰も聴いていないだけ)。
            engine.set_event_sink(move |ev| {
                let _ = sink_tx.send(ev);
            });
            let master = engine.master_bus();
            run_loop(&mut engine, master, &rx, reaper_rx);
        })?;

    match init_rx.recv() {
        Ok(Ok(())) => Ok(EngineHandle {
            tx,
            events: events_tx,
        }),
        Ok(Err(msg)) => Err(io::Error::other(msg)),
        Err(_) => Err(io::Error::other("engine thread exited during init")),
    }
}

/// エンジンスレッドの本体ループ。要求処理と `poll_events()` を交互に回す。
/// ロード済みミキサーの状態 (`MixerState`) はこのループのローカルとして所有する。
fn run_loop(
    engine: &mut SoundEngine,
    master: EntityId,
    rx: &crossbeam_channel::Receiver<EngineRequest>,
    mut despawn_rx: broadcast::Receiver<Event>,
) {
    let mut mixer_state: Option<MixerState> = None;
    // Play (clip 付き) がソースに生やしたエフェクト。ソース despawn 後に
    // remove_effect で回収する (core は source 対象エフェクトを自動解放しない)。
    let mut clip_effects: std::collections::HashMap<EntityId, Vec<nezia_core::EffectId>> =
        std::collections::HashMap::new();
    loop {
        match rx.recv_timeout(POLL_INTERVAL) {
            Ok(req) => {
                handle(engine, master, &mut mixer_state, &mut clip_effects, req);
                // 終了したソースのスロット回収 / コールバック処理。
                engine.poll_events();
            }
            Err(RecvTimeoutError::Timeout) => engine.poll_events(),
            // 全 Sender が drop された (daemon 終了) → ループを抜けてエンジンを破棄。
            Err(RecvTimeoutError::Disconnected) => break,
        }
        reap_clip_effects(engine, &mut clip_effects, &mut despawn_rx);
    }
}

/// ClipParams から spawn 時パラメータ (priority, SpawnSpatialInit) を組み立てる。
///
/// proto3 のゼロ値と実用デフォルトのずれをここで吸収する:
/// - priority 0 はデフォルト 128 として扱う (proto コメントに明記)
/// - min/max_distance / rolloff の 0 は core のデフォルト値に置き換える
fn clip_spawn_params(clip: Option<&ClipParams>) -> (u8, SpawnSpatialInit) {
    let Some(clip) = clip else {
        return (PREVIEW_PRIORITY, SpawnSpatialInit::NONE);
    };
    let priority = if clip.priority == 0 {
        PREVIEW_PRIORITY
    } else {
        clip.priority.min(255) as u8
    };
    let spatial = match &clip.spatial {
        None => SpawnSpatialInit::NONE,
        Some(s) => {
            use crate::proto::v1::AttenuationModel as ProtoModel;
            use nezia_core::AttenuationModel;
            let model = match ProtoModel::try_from(s.model) {
                Ok(ProtoModel::None) => AttenuationModel::None,
                Ok(ProtoModel::Linear) => AttenuationModel::Linear,
                Ok(ProtoModel::Exponential) => AttenuationModel::Exponential,
                _ => AttenuationModel::InverseDistance,
            };
            SpawnSpatialInit {
                enabled: true,
                model,
                min_distance: if s.min_distance > 0.0 { s.min_distance } else { 1.0 },
                max_distance: if s.max_distance > 0.0 { s.max_distance } else { 500.0 },
                rolloff: if s.rolloff > 0.0 { s.rolloff } else { 1.0 },
                doppler_level: s.doppler_level.clamp(0.0, 1.0),
                ..SpawnSpatialInit::NONE
            }
        }
    };
    (priority, spatial)
}

/// despawn したソースのクリップエフェクトを回収する。
///
/// event sink → broadcast 経由で `SourceDespawned` を受け取り、該当ソースに
/// 生やしたエフェクトを `remove_effect` する。broadcast が Lagged した場合
/// (容量 256 超のバースト) は取りこぼす可能性があるが、preview 用途の
/// イベントレートでは実質発生しない。
fn reap_clip_effects(
    engine: &mut SoundEngine,
    clip_effects: &mut std::collections::HashMap<EntityId, Vec<nezia_core::EffectId>>,
    despawn_rx: &mut broadcast::Receiver<Event>,
) {
    loop {
        match despawn_rx.try_recv() {
            Ok(Event::SourceDespawned { id }) => {
                if let Some(ids) = clip_effects.remove(&id) {
                    for effect in ids {
                        let _ = engine.remove_effect(effect);
                    }
                }
            }
            Ok(_) => {}
            Err(broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => break, // Empty / Closed
        }
    }
}

fn handle(
    engine: &mut SoundEngine,
    master: EntityId,
    mixer_state: &mut Option<MixerState>,
    clip_effects: &mut std::collections::HashMap<EntityId, Vec<nezia_core::EffectId>>,
    req: EngineRequest,
) {
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
            bus,
            clip,
            reply,
        } => {
            // バス名の解決。未指定は Master、指定はロード済みミキサーから引く。
            let target_bus = match bus.as_deref() {
                None | Some("") => Some(master),
                Some(name) => mixer_state.as_ref().and_then(|m| m.resolve(name)),
            };
            let Some(target_bus) = target_bus else {
                let _ = reply.send(PlayReply::UnknownBus);
                return;
            };
            let (priority, spatial) = clip_spawn_params(clip.as_ref());
            let handle = engine.play_with_handle(
                buffer, volume, pitch, target_bus, looping, priority, spatial,
            );
            // クリップのエフェクト / Send を spawn 済みソースへ適用する。
            if let (Some(source), Some(clip)) = (handle, clip.as_ref())
                && !(clip.effects.is_empty() && clip.sends.is_empty())
            {
                match mixer::apply_clip(engine, source, clip, mixer_state.as_ref()) {
                    Ok(ids) => {
                        if !ids.is_empty() {
                            clip_effects.insert(source, ids);
                        }
                    }
                    Err(msg) => {
                        // apply_clip がソース停止まで巻き戻し済み。
                        let _ = reply.send(PlayReply::ClipInvalid(msg));
                        return;
                    }
                }
            }
            let _ = reply.send(PlayReply::Source(handle));
        }
        EngineRequest::Stop { source, reply } => {
            let _ = reply.send(engine.stop_source(source));
        }
        EngineRequest::StopAll { reply } => {
            let _ = reply.send(engine.stop_all());
        }
        EngineRequest::LoadMixer { def, reply } => {
            // 再ロード: 既存構成を破棄してから構築する (hot reload は 0.2.0 非対応)。
            if let Some(prev) = mixer_state.take() {
                mixer::destroy_mixer(engine, prev);
            }
            let result = match mixer::build_mixer(engine, &def) {
                Ok(state) => {
                    let buses = state.named_buses().to_vec();
                    *mixer_state = Some(state);
                    Ok(buses)
                }
                Err(msg) => Err(msg),
            };
            let _ = reply.send(result);
        }
    }
}
