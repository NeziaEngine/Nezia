use crate::bus::{MAX_SENDS, SendDestKind, SendId, SendPosition};
use crate::core::sparse_set::SparseSet;
use crate::effect::{EffectId, MAX_EFFECTS_PER_SOURCE};
use crate::entity::EntityId;

use super::{MAX_SENDS_PER_SOURCE, MAX_SOURCES};

/// SendId.index → (source_dense, slot) の逆引きエントリ (source 起点 send 用)。
///
/// `BusWorld::SendLookup` と同形だが、source 用と bus 用は別 SendId 集合 (= 別 entity owner)
/// を扱うため、audio thread の SetSendGain / RemoveSend ハンドラは「先に bus を引き、
/// 見つからなければ source を引く」二段引きで dispatch する。`source_dense == u32::MAX`
/// で未割当を表す。
#[derive(Copy, Clone, Debug)]
pub(super) struct SourceSendLookup {
    pub(super) source_dense: u32,
    pub(super) slot: u8,
    pub(super) generation: u32,
}

impl SourceSendLookup {
    pub(super) const EMPTY: SourceSendLookup = SourceSendLookup {
        source_dense: u32::MAX,
        slot: 0,
        generation: 0,
    };

    pub(super) fn is_empty(&self) -> bool {
        self.source_dense == u32::MAX
    }
}

/// Source 生成時の初期パラメータ。
pub struct SourceComponent {
    pub vol: f32,
    pub pitch: f32,
    pub sample_offset: f32,
    /// 再生する AudioBuffer のインデックス。
    pub audio_buffer_index: u32,
    /// 出力先バスの密配列インデックス。0 = マスターバス。
    pub output_bus: u32,
    /// コールバックトークン。0 = コールバックなし。
    pub token: u32,
    /// ループ再生フラグ。`true` の場合、バッファ末尾到達時に先頭へ巻き戻す。
    pub looping: bool,
    /// Voice Virtualization 用優先度。Wwise / CRI ADX2 互換 (0..255、**高いほど高優先**)。
    /// 既定値 128 (中央値)。Wwise は 0..100、ADX2 は 0..255 だが、いずれも「高い値ほど重要」が共通。
    pub priority: u8,
    /// Phase 3-4: 予約再生開始時刻 (絶対 DSP frame)。
    /// `0` は「未指定 = 即時再生」のセンチネル。それ以外は engine の DSP clock
    /// (`SoundEngine::dsp_time_samples`) 基準で発音開始する frame。
    pub start_dsp_frame: u64,
}

impl Default for SourceComponent {
    fn default() -> Self {
        Self {
            vol: 1.0,
            pitch: 1.0,
            sample_offset: 0.0,
            audio_buffer_index: 0,
            output_bus: 0,
            token: 0,
            looping: false,
            priority: 128,
            start_dsp_frame: 0,
        }
    }
}

/// Source の再生状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState {
    /// 再生中。
    Playing,
    /// 一時停止中。再開可能。
    Pausing,
    /// 停止済み。次の update で despawn される。
    Stopped,
    /// Phase 3-4: 発音待機中 (PlayScheduled)。
    /// `start_dsp_frame > clock_at_callback_start` の間は mixing をスキップし、
    /// virtualizer のスコアリング対象外 (Playing でないため自然に除外される)。
    /// callback 冒頭で `start_dsp_frame <= clock_at_callback_start + frames_in_callback`
    /// になった時点で `Playing` へ遷移し、必要なら sub-callback offset を伴って発音する。
    Scheduled,
}

/// Source ワールド。
///
/// スパースセット方式の SoA（Structure of Arrays）レイアウトで
/// Source ごとのコンポーネントを管理する。
/// 各コンポーネント（vol, pitch, sample_offset）は独立した密配列に格納され、
/// キャッシュ効率の高い一括処理が可能。
pub struct SourceWorld {
    // ── エンティティ管理 ──
    entities: SparseSet,

    // ── 密配列（dense arrays / コンポーネント） ──
    /// 音量（0.0〜1.0）。
    pub(super) vol: Vec<f32>,
    /// ピッチ倍率（1.0 = 原音、2.0 = 1オクターブ上）。
    pub(super) pitch: Vec<f32>,
    /// サンプルオフセット（再生位置）。
    pub(super) sample_offset: Vec<f32>,
    /// 再生する AudioBuffer のインデックス。
    pub(super) audio_buffer_index: Vec<u32>,
    /// 再生状態。
    pub(super) state: Vec<SourceState>,
    /// 出力先バスの密配列インデックス。
    pub(super) output_bus: Vec<u32>,
    /// コールバックトークン。0 = コールバックなし。
    pub(super) token: Vec<u32>,
    /// ループ再生フラグ。
    pub(super) looping: Vec<bool>,
    /// Voice Virtualization 用優先度 (0..255, **高いほど高優先**, Wwise / ADX2 互換)。
    pub(super) priority: Vec<u8>,
    /// Voice Virtualization タグ。`true` のソースはミキシング段でスキップされ、
    /// `sample_offset` だけ前進する (時間同期維持)。
    /// 毎フレーム冒頭の rebalance で再評価される。
    pub(super) is_virtual: Vec<bool>,

    // ── DSP Pre-Spatial エフェクトチェーン (Phase 2-3) ──
    /// Pre-Spatial エフェクトチェーン (resampler 後、Spatial 適用前のモノラル信号に作用)。
    pub(super) pre_chain: Vec<[EffectId; MAX_EFFECTS_PER_SOURCE]>,
    pub(super) pre_count: Vec<u8>,

    // ── 予約再生 (Phase 3-4) ──
    /// 予約再生開始時刻 (絶対 DSP frame)。`0` で即時再生。
    /// `state == Scheduled` のソースは `start_dsp_frame > clock_at_callback_start` の間
    /// 発音されず待機する。
    pub(super) start_dsp_frame: Vec<u64>,

    // ── Source 起点 Send (User-Defined Aux Send) ──
    /// Wwise / FMOD 互換の per-event aux send。各ソースから他バス / Compressor sidechain
    /// への副ルートを最大 `MAX_SENDS_PER_SOURCE` 本まで張れる。バス起点 Send (`BusWorld::send_*`)
    /// と SoA 形式は揃えてあるが、SendId プールは共通なので audio thread のハンドラは
    /// 両方を見て dispatch する。
    pub(super) send_dest_dense: Vec<[u32; MAX_SENDS_PER_SOURCE]>,
    pub(super) send_gain: Vec<[f32; MAX_SENDS_PER_SOURCE]>,
    pub(super) send_position: Vec<[u8; MAX_SENDS_PER_SOURCE]>,
    pub(super) send_dest_kind: Vec<[u8; MAX_SENDS_PER_SOURCE]>,
    pub(super) send_id: Vec<[SendId; MAX_SENDS_PER_SOURCE]>,
    pub(super) send_count: Vec<u8>,
    /// SendId.index → (source_dense, slot) の逆引き。サイズは `MAX_SENDS` で固定。
    /// generation 不一致は stale 扱い。
    send_lookup: Vec<SourceSendLookup>,
}

impl Default for SourceWorld {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
impl SourceWorld {
    pub fn new() -> Self {
        Self {
            entities: SparseSet::new(MAX_SOURCES),
            vol: Vec::with_capacity(MAX_SOURCES),
            pitch: Vec::with_capacity(MAX_SOURCES),
            sample_offset: Vec::with_capacity(MAX_SOURCES),
            audio_buffer_index: Vec::with_capacity(MAX_SOURCES),
            state: Vec::with_capacity(MAX_SOURCES),
            output_bus: Vec::with_capacity(MAX_SOURCES),
            token: Vec::with_capacity(MAX_SOURCES),
            looping: Vec::with_capacity(MAX_SOURCES),
            priority: Vec::with_capacity(MAX_SOURCES),
            is_virtual: Vec::with_capacity(MAX_SOURCES),
            pre_chain: Vec::with_capacity(MAX_SOURCES),
            pre_count: Vec::with_capacity(MAX_SOURCES),
            start_dsp_frame: Vec::with_capacity(MAX_SOURCES),
            send_dest_dense: Vec::with_capacity(MAX_SOURCES),
            send_gain: Vec::with_capacity(MAX_SOURCES),
            send_position: Vec::with_capacity(MAX_SOURCES),
            send_dest_kind: Vec::with_capacity(MAX_SOURCES),
            send_id: Vec::with_capacity(MAX_SOURCES),
            send_count: Vec::with_capacity(MAX_SOURCES),
            send_lookup: vec![SourceSendLookup::EMPTY; MAX_SENDS],
        }
    }

    /// 内部用: spawn 時に send 関連 dense 配列を空状態で push する。
    fn push_empty_send_components(&mut self) {
        self.send_dest_dense.push([0; MAX_SENDS_PER_SOURCE]);
        self.send_gain.push([0.0; MAX_SENDS_PER_SOURCE]);
        self.send_position.push([0; MAX_SENDS_PER_SOURCE]);
        self.send_dest_kind.push([0; MAX_SENDS_PER_SOURCE]);
        self.send_id.push([SendId::INVALID; MAX_SENDS_PER_SOURCE]);
        self.send_count.push(0);
    }

    /// 内部用: spawn 失敗時に push 済み send 関連 dense 配列を巻き戻す。
    fn pop_send_components(&mut self) {
        self.send_dest_dense.pop();
        self.send_gain.pop();
        self.send_position.pop();
        self.send_dest_kind.pop();
        self.send_id.pop();
        self.send_count.pop();
    }

    /// Source を追加し、EntityId を返す（fire-and-forget 用）。
    ///
    /// `MAX_SOURCES` に達している場合は `None` を返す。
    pub fn spawn(&mut self, params: SourceComponent) -> Option<EntityId> {
        let (id, _dense) = self.entities.alloc()?;
        self.vol.push(params.vol);
        self.pitch.push(params.pitch);
        self.sample_offset.push(params.sample_offset);
        self.audio_buffer_index.push(params.audio_buffer_index);
        // Phase 3-4: start_dsp_frame == 0 は即時再生のセンチネル。それ以外は
        // Scheduled としてミキシングを待機させ、audio thread の `activate_scheduled`
        // で実 DSP clock と比較して Playing 化する (過去指定もそこで吸収する)。
        let initial_state = if params.start_dsp_frame == 0 {
            SourceState::Playing
        } else {
            SourceState::Scheduled
        };
        self.state.push(initial_state);
        self.output_bus.push(params.output_bus);
        self.token.push(params.token);
        self.looping.push(params.looping);
        self.priority.push(params.priority);
        // 仮想化判定は次のフレーム冒頭で行う。spawn 時点では物理として登録。
        self.is_virtual.push(false);
        self.pre_chain.push(
            [EffectId {
                index: 0,
                generation: 0,
            }; MAX_EFFECTS_PER_SOURCE],
        );
        self.pre_count.push(0);
        self.start_dsp_frame.push(params.start_dsp_frame);
        self.push_empty_send_components();
        Some(id)
    }

    /// 事前割り当てされた EntityId を使って Source をスポーンする（3D ソース用）。
    ///
    /// 同じ index が既に使用中の場合は `false` を返す。
    /// メインスレッドが EntityId を事前発行し、`SpawnSource` コマンドで渡す想定。
    pub fn spawn_with_id(&mut self, id: EntityId, params: SourceComponent) -> bool {
        let Some(_dense) = self.entities.alloc_with_id(id) else {
            return false;
        };
        self.vol.push(params.vol);
        self.pitch.push(params.pitch);
        self.sample_offset.push(params.sample_offset);
        self.audio_buffer_index.push(params.audio_buffer_index);
        let initial_state = if params.start_dsp_frame == 0 {
            SourceState::Playing
        } else {
            SourceState::Scheduled
        };
        self.state.push(initial_state);
        self.output_bus.push(params.output_bus);
        self.token.push(params.token);
        self.looping.push(params.looping);
        self.priority.push(params.priority);
        self.is_virtual.push(false);
        self.pre_chain.push(
            [EffectId {
                index: 0,
                generation: 0,
            }; MAX_EFFECTS_PER_SOURCE],
        );
        self.pre_count.push(0);
        self.start_dsp_frame.push(params.start_dsp_frame);
        self.push_empty_send_components();
        true
    }

    /// EntityId を検証し、有効なら密配列インデックスを返す。
    pub fn resolve(&self, id: EntityId) -> Option<usize> {
        self.entities.resolve(id)
    }

    /// Source を削除する（swap-remove）。
    pub fn despawn(&mut self, id: EntityId) -> bool {
        let Some(dense_index) = self.entities.dealloc(id) else {
            return false;
        };
        self.swap_remove_dense(dense_index);
        true
    }

    /// 内部用: dense_index の SoA 全フィールドを swap_remove し、send_lookup を整合させる。
    fn swap_remove_dense(&mut self, dense_index: usize) {
        // 1. 当該ソースから出ていた send の lookup を全クリア (本体は後で swap_remove)。
        let originating_count = self.send_count[dense_index] as usize;
        for slot in 0..originating_count {
            let sid = self.send_id[dense_index][slot];
            if sid.is_valid() && (sid.index as usize) < self.send_lookup.len() {
                let lk = &self.send_lookup[sid.index as usize];
                if lk.generation == sid.generation {
                    self.send_lookup[sid.index as usize] = SourceSendLookup::EMPTY;
                }
            }
        }

        let last_dense = self.vol.len() - 1;
        let dense_u32 = dense_index as u32;
        let last_u32 = last_dense as u32;

        // 2. SoA 列を swap_remove。
        self.vol.swap_remove(dense_index);
        self.pitch.swap_remove(dense_index);
        self.sample_offset.swap_remove(dense_index);
        self.audio_buffer_index.swap_remove(dense_index);
        self.state.swap_remove(dense_index);
        self.output_bus.swap_remove(dense_index);
        self.token.swap_remove(dense_index);
        self.looping.swap_remove(dense_index);
        self.priority.swap_remove(dense_index);
        self.is_virtual.swap_remove(dense_index);
        self.pre_chain.swap_remove(dense_index);
        self.pre_count.swap_remove(dense_index);
        self.start_dsp_frame.swap_remove(dense_index);
        self.send_dest_dense.swap_remove(dense_index);
        self.send_gain.swap_remove(dense_index);
        self.send_position.swap_remove(dense_index);
        self.send_dest_kind.swap_remove(dense_index);
        self.send_id.swap_remove(dense_index);
        self.send_count.swap_remove(dense_index);

        // 3. 末尾にあった source が dense_index に移動したので send_lookup を更新。
        if dense_index != last_dense && dense_index < self.send_count.len() {
            let n = self.send_count[dense_index] as usize;
            for slot in 0..n {
                let sid = self.send_id[dense_index][slot];
                if sid.is_valid() && (sid.index as usize) < self.send_lookup.len() {
                    let lk = &mut self.send_lookup[sid.index as usize];
                    if lk.source_dense == last_u32 && lk.generation == sid.generation {
                        lk.source_dense = dense_u32;
                    }
                }
            }
        }
    }

    /// EntityId が有効か確認する。
    pub fn contains(&self, id: EntityId) -> bool {
        self.entities.contains(id)
    }

    /// 現在の Source 数。
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    // ── 個別アクセス ──

    pub fn vol(&self, id: EntityId) -> Option<f32> {
        self.resolve(id).map(|i| self.vol[i])
    }

    pub fn pitch(&self, id: EntityId) -> Option<f32> {
        self.resolve(id).map(|i| self.pitch[i])
    }

    pub fn sample_offset(&self, id: EntityId) -> Option<f32> {
        self.resolve(id).map(|i| self.sample_offset[i])
    }

    pub fn set_vol(&mut self, id: EntityId, value: f32) -> bool {
        if let Some(i) = self.resolve(id) {
            self.vol[i] = value;
            true
        } else {
            false
        }
    }

    pub fn set_pitch(&mut self, id: EntityId, value: f32) -> bool {
        if let Some(i) = self.resolve(id) {
            self.pitch[i] = value;
            true
        } else {
            false
        }
    }

    pub fn set_sample_offset(&mut self, id: EntityId, value: f32) -> bool {
        if let Some(i) = self.resolve(id) {
            self.sample_offset[i] = value;
            true
        } else {
            false
        }
    }

    pub fn looping(&self, id: EntityId) -> Option<bool> {
        self.resolve(id).map(|i| self.looping[i])
    }

    pub fn set_looping(&mut self, id: EntityId, value: bool) -> bool {
        if let Some(i) = self.resolve(id) {
            self.looping[i] = value;
            true
        } else {
            false
        }
    }

    pub fn state(&self, id: EntityId) -> Option<SourceState> {
        self.resolve(id).map(|i| self.state[i])
    }

    pub fn set_state(&mut self, id: EntityId, value: SourceState) -> bool {
        if let Some(i) = self.resolve(id) {
            self.state[i] = value;
            true
        } else {
            false
        }
    }

    // ── 一括アクセス（密配列スライス） ──

    pub fn vols(&self) -> &[f32] {
        &self.vol
    }

    pub fn vols_mut(&mut self) -> &mut [f32] {
        &mut self.vol
    }

    /// 密配列の (entity, sample_offset) を順に走査するイテレータ。
    pub fn snapshots(&self) -> impl Iterator<Item = (EntityId, f32)> + '_ {
        let len = self.sample_offset.len();
        (0..len).filter_map(move |dense| {
            let id = self.entities.entity_at_dense(dense)?;
            Some((id, self.sample_offset[dense]))
        })
    }

    /// dense index に対応する EntityId を取得する。
    pub fn entity_at_dense(&self, dense_index: usize) -> Option<EntityId> {
        self.entities.entity_at_dense(dense_index)
    }

    /// volume / pitch の dense 配列に直接書き込む。
    ///
    /// `apply_live_params` 等で SoA 一括反映する際に使う。
    pub fn write_vol(&mut self, dense_index: usize, value: f32) {
        if dense_index < self.vol.len() {
            self.vol[dense_index] = value;
        }
    }

    pub fn write_pitch(&mut self, dense_index: usize, value: f32) {
        if dense_index < self.pitch.len() {
            self.pitch[dense_index] = value;
        }
    }

    /// 密配列インデックスを指定して Source を削除する（swap-remove）。
    ///
    /// `SourceMixingSystem::update()` が再生終了した Source を直接削除するために使用する。
    /// 逆順で呼び出すこと（swap-remove のため後ろから消さないとインデックスがずれる）。
    pub(super) fn despawn_by_dense_index(&mut self, dense_index: usize) {
        if !self.entities.dealloc_by_dense_index(dense_index) {
            return;
        }
        self.swap_remove_dense(dense_index);
    }

    // ── Voice Virtualization アクセサ ────────────────────────────────

    pub fn priority(&self, id: EntityId) -> Option<u8> {
        self.resolve(id).map(|i| self.priority[i])
    }

    pub fn set_priority(&mut self, id: EntityId, value: u8) -> bool {
        if let Some(i) = self.resolve(id) {
            self.priority[i] = value;
            true
        } else {
            false
        }
    }

    pub fn is_virtual(&self, id: EntityId) -> Option<bool> {
        self.resolve(id).map(|i| self.is_virtual[i])
    }

    /// SoA 一括アクセス (`SourceMixingSystem` / rebalance 用)。
    pub fn priorities(&self) -> &[u8] {
        &self.priority
    }
    pub fn is_virtuals(&self) -> &[bool] {
        &self.is_virtual
    }
    pub fn is_virtuals_mut(&mut self) -> &mut [bool] {
        &mut self.is_virtual
    }
    pub fn states(&self) -> &[SourceState] {
        &self.state
    }
    /// SoA 一括書き換え用 (audio thread の `activate_scheduled` で `Scheduled → Playing` 遷移に使う)。
    pub fn states_mut(&mut self) -> &mut [SourceState] {
        &mut self.state
    }
    /// 予約再生開始時刻 (絶対 DSP frame) の dense 配列。
    pub fn start_dsp_frames(&self) -> &[u64] {
        &self.start_dsp_frame
    }
    pub fn pitches(&self) -> &[f32] {
        &self.pitch
    }
    pub fn output_buses(&self) -> &[u32] {
        &self.output_bus
    }

    // ── DSP Pre-Spatial エフェクトチェーン操作 (Phase 2-3) ──

    /// ソースの Pre-Spatial チェーン末尾に EffectId を追加する。
    /// 戻り値: 挿入された slot index。チェーン満杯時は `None`。
    pub fn push_pre_effect(&mut self, source_dense: usize, eff: EffectId) -> Option<u8> {
        if source_dense >= self.pre_count.len() {
            return None;
        }
        let idx = self.pre_count[source_dense] as usize;
        if idx >= MAX_EFFECTS_PER_SOURCE {
            return None;
        }
        self.pre_chain[source_dense][idx] = eff;
        self.pre_count[source_dense] += 1;
        Some(idx as u8)
    }

    /// `eff` をチェーンから削除し後続を詰める。見つかれば `true`。
    pub fn remove_pre_effect(&mut self, source_dense: usize, eff: EffectId) -> bool {
        if source_dense >= self.pre_count.len() {
            return false;
        }
        let n = self.pre_count[source_dense] as usize;
        let chain = &mut self.pre_chain[source_dense];
        for i in 0..n {
            if chain[i] == eff {
                for j in i..n - 1 {
                    chain[j] = chain[j + 1];
                }
                self.pre_count[source_dense] -= 1;
                return true;
            }
        }
        false
    }

    pub fn pre_chain_slice(&self, source_dense: usize) -> &[EffectId] {
        let n = self.pre_count[source_dense] as usize;
        &self.pre_chain[source_dense][..n]
    }

    // ── Source 起点 Send (User-Defined Aux Send) ──

    /// 指定ソースに send を追加する。chain 満杯または `source_dense` 範囲外で `false`。
    /// メインスレッドで `id` を事前発行済みの前提。
    pub fn add_send(
        &mut self,
        source_dense: usize,
        id: SendId,
        dest_dense: u32,
        dest_kind: SendDestKind,
        gain: f32,
        position: SendPosition,
    ) -> bool {
        if source_dense >= self.send_count.len() {
            return false;
        }
        let count = self.send_count[source_dense] as usize;
        if count >= MAX_SENDS_PER_SOURCE {
            return false;
        }
        if (id.index as usize) >= self.send_lookup.len() {
            return false;
        }
        self.send_dest_dense[source_dense][count] = dest_dense;
        self.send_gain[source_dense][count] = gain;
        self.send_position[source_dense][count] = position as u8;
        self.send_dest_kind[source_dense][count] = dest_kind as u8;
        self.send_id[source_dense][count] = id;
        self.send_count[source_dense] = (count + 1) as u8;
        self.send_lookup[id.index as usize] = SourceSendLookup {
            source_dense: source_dense as u32,
            slot: count as u8,
            generation: id.generation,
        };
        true
    }

    /// SendId で send を削除する。stale (generation 不一致) または未存在で `false`。
    pub fn remove_send(&mut self, id: SendId) -> bool {
        let Some((source_dense, slot)) = self.resolve_send(id) else {
            return false;
        };
        self.swap_remove_send_slot(source_dense, slot);
        true
    }

    /// SendId の現在位置 `(source_dense, slot)` を返す。stale なら `None`。
    pub fn resolve_send(&self, id: SendId) -> Option<(usize, usize)> {
        if !id.is_valid() {
            return None;
        }
        let lookup_idx = id.index as usize;
        let lk = self.send_lookup.get(lookup_idx)?;
        if lk.is_empty() || lk.generation != id.generation {
            return None;
        }
        Some((lk.source_dense as usize, lk.slot as usize))
    }

    /// SendId の gain を設定する。
    pub fn set_send_gain(&mut self, id: SendId, gain: f32) -> bool {
        if let Some((source_dense, slot)) = self.resolve_send(id) {
            self.send_gain[source_dense][slot] = gain;
            true
        } else {
            false
        }
    }

    /// SendId のタップ位置を変更する。
    pub fn set_send_position(&mut self, id: SendId, position: SendPosition) -> bool {
        if let Some((source_dense, slot)) = self.resolve_send(id) {
            self.send_position[source_dense][slot] = position as u8;
            true
        } else {
            false
        }
    }

    /// dense index 指定で send_gain を直接書き込む (Snapshot 補間で使用)。
    #[inline]
    pub fn write_send_gain_by_dense(&mut self, source_dense: usize, slot: usize, gain: f32) {
        if let Some(arr) = self.send_gain.get_mut(source_dense)
            && slot < MAX_SENDS_PER_SOURCE
        {
            arr[slot] = gain;
        }
    }

    /// dense index 指定で send_gain を読み出す (Snapshot apply 時の現在値キャプチャ用)。
    #[inline]
    #[must_use]
    pub fn send_gain_at(&self, source_dense: usize, slot: usize) -> Option<f32> {
        self.send_gain
            .get(source_dense)
            .and_then(|arr| arr.get(slot))
            .copied()
    }

    /// ソース `source_dense` の send 数。
    #[inline]
    pub fn send_count_at(&self, source_dense: usize) -> usize {
        self.send_count
            .get(source_dense)
            .copied()
            .map(|c| c as usize)
            .unwrap_or(0)
    }

    /// ソース `source_dense` の slot `slot` にある send 情報 `(dest_dense, gain, position, dest_kind)`。
    #[inline]
    pub fn send_at(&self, source_dense: usize, slot: usize) -> (u32, f32, u8, u8) {
        (
            self.send_dest_dense[source_dense][slot],
            self.send_gain[source_dense][slot],
            self.send_position[source_dense][slot],
            self.send_dest_kind[source_dense][slot],
        )
    }

    /// 内部用: ソース `source_dense` の slot を swap-remove し send_lookup を整合させる。
    fn swap_remove_send_slot(&mut self, source_dense: usize, slot: usize) {
        let count = self.send_count[source_dense] as usize;
        debug_assert!(slot < count);

        // 除去対象の lookup をクリア。
        let removed_sid = self.send_id[source_dense][slot];
        if removed_sid.is_valid() && (removed_sid.index as usize) < self.send_lookup.len() {
            let lk = &self.send_lookup[removed_sid.index as usize];
            if lk.generation == removed_sid.generation {
                self.send_lookup[removed_sid.index as usize] = SourceSendLookup::EMPTY;
            }
        }

        let last = count - 1;
        if slot != last {
            self.send_dest_dense[source_dense][slot] = self.send_dest_dense[source_dense][last];
            self.send_gain[source_dense][slot] = self.send_gain[source_dense][last];
            self.send_position[source_dense][slot] = self.send_position[source_dense][last];
            self.send_dest_kind[source_dense][slot] = self.send_dest_kind[source_dense][last];
            self.send_id[source_dense][slot] = self.send_id[source_dense][last];

            // 移動した send の lookup slot を更新。
            let moved_sid = self.send_id[source_dense][slot];
            if moved_sid.is_valid() && (moved_sid.index as usize) < self.send_lookup.len() {
                let lk = &mut self.send_lookup[moved_sid.index as usize];
                if lk.source_dense == source_dense as u32 && lk.generation == moved_sid.generation {
                    lk.slot = slot as u8;
                }
            }
        }
        self.send_count[source_dense] -= 1;
    }
}
