use crate::entity::EntityId;

/// 最大同時発音数。
pub const MAX_VOICES: usize = 256;

/// ボイスプール。
///
/// スパースセット方式の SoA（Structure of Arrays）レイアウトで
/// ボイスごとのコンポーネントを管理する。
/// 各コンポーネント（vol, pitch, sample_offset）は独立した密配列に格納され、
/// キャッシュ効率の高い一括処理が可能。
///
/// ## C# との対比: AoS vs SoA
///
/// C# で素直に書くと AoS（Array of Structures）になる:
///
/// ```csharp
/// // AoS — オブジェクトごとにデータがまとまる（C# の自然な形）
/// class Voice { public float Vol; public float Pitch; public float SampleOffset; }
/// List<Voice> voices; // メモリ: [Vol,Pitch,Offset, Vol,Pitch,Offset, ...]
/// ```
///
/// この VoicePool は SoA（Structure of Arrays）で、コンポーネント種別ごとに
/// 別々の配列を持つ:
///
/// ```csharp
/// // SoA — プロパティごとに配列が分かれる（この実装の方式）
/// List<float> vols;           // メモリ: [Vol, Vol, Vol, ...]
/// List<float> pitches;        // メモリ: [Pitch, Pitch, Pitch, ...]
/// List<float> sampleOffsets;  // メモリ: [Offset, Offset, Offset, ...]
/// ```
///
/// 「全ボイスの音量だけ一括処理」のようなケースで、SoA は
/// 不要なフィールド (pitch 等) をキャッシュラインに載せずに済むため高速。
pub struct VoicePool {
    // ── 疎配列（sparse array） ──
    // C# で言うと: Dictionary<int, int> のようなマッピングだが、
    // 配列の添字で直接引けるので Dictionary より高速（O(1) guaranteed）。

    /// EntityId.index → 密配列インデックスへのマッピング。
    sparse: Vec<Option<SparseEntry>>,
    /// 密配列インデックス → EntityId.index への逆マッピング。
    dense_to_sparse: Vec<u32>,

    // ── 密配列（dense arrays / コンポーネント） ──
    // C# の List<float> に相当。要素は隙間なく詰まっている。

    /// 音量（0.0〜1.0）。
    vol: Vec<f32>,
    /// ピッチ倍率（1.0 = 原音、2.0 = 1オクターブ上）。
    pitch: Vec<f32>,
    /// サンプルオフセット（再生位置）。
    sample_offset: Vec<f32>,

    // ── スロット管理 ──
    // C# の Stack<int> に相当。削除済みインデックスを再利用する。
    free_list: Vec<u32>,
    next_index: u32,
}

#[derive(Debug, Clone, Copy)]
struct SparseEntry {
    dense_index: u32,
    generation: u32,
}

/// ボイス生成時の初期パラメータ。
pub struct VoiceParams {
    pub vol: f32,
    pub pitch: f32,
    pub sample_offset: f32,
}

impl Default for VoiceParams {
    fn default() -> Self {
        Self {
            vol: 1.0,
            pitch: 1.0,
            sample_offset: 0.0,
        }
    }
}

impl Default for VoicePool {
    fn default() -> Self {
        Self::new()
    }
}

impl VoicePool {
    pub fn new() -> Self {
        Self {
            sparse: Vec::with_capacity(MAX_VOICES),
            dense_to_sparse: Vec::with_capacity(MAX_VOICES),
            vol: Vec::with_capacity(MAX_VOICES),
            pitch: Vec::with_capacity(MAX_VOICES),
            sample_offset: Vec::with_capacity(MAX_VOICES),
            free_list: Vec::with_capacity(MAX_VOICES),
            next_index: 0,
        }
    }

    /// ボイスを追加し、EntityId を返す。
    ///
    /// ## 処理の流れ
    ///
    /// 1. free_list に再利用可能なスロットがあればそれを使う（generation は既にインクリメント済み）
    /// 2. なければ新しい index を発行する
    /// 3. 各密配列の末尾にコンポーネントを push する
    ///
    /// ## C# で書くと
    ///
    /// ```csharp
    /// public EntityId Spawn(VoiceParams p) {
    ///     int denseIndex = _vols.Count;
    ///     uint index, generation;
    ///     if (_freeList.Count > 0) {
    ///         index = _freeList.Pop();
    ///         generation = _sparse[index].Generation; // 再利用時は前回 +1 済み
    ///     } else {
    ///         index = _nextIndex++;
    ///         generation = 0;
    ///     }
    ///     _sparse[index] = new SparseEntry(denseIndex, generation);
    ///     _vols.Add(p.Vol);
    ///     _pitches.Add(p.Pitch);
    ///     _sampleOffsets.Add(p.SampleOffset);
    ///     return new EntityId(index, generation);
    /// }
    /// ```
    /// `MAX_VOICES` に達している場合は `None` を返す。
    pub fn spawn(&mut self, params: VoiceParams) -> Option<EntityId> {
        if self.vol.len() >= MAX_VOICES {
            return None;
        }
        let dense_index = self.vol.len() as u32;

        let (index, generation) = if let Some(reused) = self.free_list.pop() {
            let reused_gen = self.sparse[reused as usize]
                .map(|e| e.generation)
                .unwrap_or(0);
            self.sparse[reused as usize] = Some(SparseEntry {
                dense_index,
                generation: reused_gen,
            });
            (reused, reused_gen)
        } else {
            let index = self.next_index;
            self.next_index += 1;
            if index as usize >= self.sparse.len() {
                self.sparse.resize(index as usize + 1, None);
            }
            self.sparse[index as usize] = Some(SparseEntry {
                dense_index,
                generation: 0,
            });
            (index, 0)
        };

        self.dense_to_sparse.push(index);
        self.vol.push(params.vol);
        self.pitch.push(params.pitch);
        self.sample_offset.push(params.sample_offset);

        Some(EntityId { index, generation })
    }

    /// EntityId を検証し、有効なら密配列インデックスを返す。
    ///
    /// C# で言うと `TryGetValue` + バージョンチェック:
    ///
    /// ```csharp
    /// bool TryResolve(EntityId id, out int denseIndex) {
    ///     if (_sparse[id.Index] is { } entry && entry.Generation == id.Generation) {
    ///         denseIndex = entry.DenseIndex;
    ///         return true;
    ///     }
    ///     denseIndex = -1;
    ///     return false;
    /// }
    /// ```
    ///
    /// Rust では `Option<usize>` で成否を表現する（C# の `out` + `bool` 戻り値に相当）。
    fn resolve(&self, id: EntityId) -> Option<usize> {
        let entry = self.sparse.get(id.index as usize)?.as_ref()?;
        if entry.generation != id.generation {
            return None;
        }
        Some(entry.dense_index as usize)
    }

    /// ボイスを削除する（swap-remove）。
    ///
    /// ## swap-remove とは
    ///
    /// 配列の中間要素を削除するとき、C# の `List<T>.RemoveAt(i)` は
    /// 後続要素を全部シフトするので O(n)。
    /// swap-remove は「末尾要素を削除位置にコピーして末尾を縮める」だけなので O(1)。
    /// 要素の順序は変わるが、スパースセットでは順序に意味がないので問題ない。
    ///
    /// ```csharp
    /// // C# での swap-remove イメージ
    /// void SwapRemoveAt<T>(List<T> list, int i) {
    ///     list[i] = list[^1];  // 末尾を削除位置にコピー
    ///     list.RemoveAt(list.Count - 1); // 末尾を除去（シフト不要）
    /// }
    /// ```
    pub fn despawn(&mut self, id: EntityId) -> bool {
        let Some(dense_index) = self.resolve(id) else {
            return false;
        };
        let last_dense = self.vol.len() - 1;

        // 疎エントリを無効化し generation をインクリメント。
        // 次に同じ index が再利用されたとき、古い EntityId は
        // generation が合わないので resolve() で弾かれる。
        if let Some(entry) = &mut self.sparse[id.index as usize] {
            *entry = SparseEntry {
                dense_index: 0,
                generation: entry.generation + 1,
            };
        }
        self.free_list.push(id.index);

        // swap-remove: 末尾要素を削除位置に移動し、
        // 移動した要素の疎配列エントリも更新する。
        if dense_index != last_dense {
            let moved_sparse_index = self.dense_to_sparse[last_dense];
            if let Some(entry) = &mut self.sparse[moved_sparse_index as usize] {
                entry.dense_index = dense_index as u32;
            }
            self.dense_to_sparse[dense_index] = moved_sparse_index;
        }

        // Rust の Vec::swap_remove は上記の C# SwapRemoveAt と同じ動作。
        // 全コンポーネント配列に対して同じ位置で swap-remove する。
        self.dense_to_sparse.swap_remove(dense_index);
        self.vol.swap_remove(dense_index);
        self.pitch.swap_remove(dense_index);
        self.sample_offset.swap_remove(dense_index);

        true
    }

    /// EntityId が有効か確認する。
    pub fn contains(&self, id: EntityId) -> bool {
        self.resolve(id).is_some()
    }

    /// 現在のボイス数。
    pub fn len(&self) -> usize {
        self.vol.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vol.is_empty()
    }

    // ── 個別アクセス ──
    // resolve() で EntityId → 密配列インデックスに変換してからアクセスする。
    // C# で言えば Dictionary の TryGetValue 後に List[index] するイメージ。

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

    // ── 一括アクセス（密配列スライス） ──
    // C# の `Span<float>` に近い。密配列を直接スライスで返すことで、
    // for ループでの一括処理がキャッシュフレンドリーになる。
    //
    // 例: 全ボイスの音量を半減
    //   for v in pool.vols_mut() { *v *= 0.5; }
    //
    // C# なら:
    //   foreach (ref var v in CollectionsMarshal.AsSpan(vols)) { v *= 0.5f; }

    pub fn vols(&self) -> &[f32] {
        &self.vol
    }

    pub fn vols_mut(&mut self) -> &mut [f32] {
        &mut self.vol
    }

    pub fn pitches(&self) -> &[f32] {
        &self.pitch
    }

    pub fn pitches_mut(&mut self) -> &mut [f32] {
        &mut self.pitch
    }

    pub fn sample_offsets(&self) -> &[f32] {
        &self.sample_offset
    }

    pub fn sample_offsets_mut(&mut self) -> &mut [f32] {
        &mut self.sample_offset
    }
}
