//! Send 関連の SoA 共通テーブル。
//!
//! `BusWorld` (`MAX_SENDS_PER_BUS`) と `SourceWorld` (`MAX_SENDS_PER_SOURCE`) は、
//! オーナーの dense 配列に「ソースから出ていく Send 群」を保持する SoA を持つ。
//! フィールドの並びとロジック (add / remove / resolve / swap_remove) はほぼ同形なので、
//! ここに `SendTable<const CAP: usize>` として共通化している。
//!
//! `SendId` プールは Bus / Source で共有 (audio thread の dispatcher は両方を引いて
//! どちらか一方に dispatch する)。テーブルの `lookup` 配列は owner ごとに独立しており、
//! `SendId.index → (owner_dense, slot)` の逆引きを持つ。
//!
//! 「owner」は呼出側の意味論で `bus_dense` か `source_dense` のいずれかを指す。テーブル
//! 自身は中立的に `owner_dense` と呼ぶ。

use super::send::SendId;

/// `SendId.index → (owner_dense, slot)` の逆引きエントリ。
///
/// `owner_dense == u32::MAX` で未割当を表す。`generation` は割当時の `SendId.generation`
/// を保持し、stale な操作 (slot 再利用後の古い SendId) を弾くのに使う。
#[derive(Copy, Clone, Debug)]
pub(crate) struct SendLookup {
    pub(crate) owner_dense: u32,
    pub(crate) slot: u8,
    pub(crate) generation: u32,
}

impl SendLookup {
    pub(crate) const EMPTY: SendLookup = SendLookup {
        owner_dense: u32::MAX,
        slot: 0,
        generation: 0,
    };

    pub(crate) fn is_empty(&self) -> bool {
        self.owner_dense == u32::MAX
    }
}

/// オーナーごとの Send SoA + `SendId` 逆引き。
///
/// `CAP` は per-owner の最大 Send 数 (`MAX_SENDS_PER_BUS` または `MAX_SENDS_PER_SOURCE`)。
/// `lookup` のサイズはコンストラクタで受け取る `max_sends` (= `MAX_SENDS`、SendId プール上限)。
pub(crate) struct SendTable<const CAP: usize> {
    pub(crate) dest_dense: Vec<[u32; CAP]>,
    pub(crate) gain: Vec<[f32; CAP]>,
    pub(crate) position: Vec<[u8; CAP]>,
    pub(crate) dest_kind: Vec<[u8; CAP]>,
    pub(crate) id: Vec<[SendId; CAP]>,
    pub(crate) count: Vec<u8>,
    pub(crate) lookup: Vec<SendLookup>,
}

impl<const CAP: usize> SendTable<CAP> {
    pub(crate) fn new(owner_capacity: usize, max_sends: usize) -> Self {
        Self {
            dest_dense: Vec::with_capacity(owner_capacity),
            gain: Vec::with_capacity(owner_capacity),
            position: Vec::with_capacity(owner_capacity),
            dest_kind: Vec::with_capacity(owner_capacity),
            id: Vec::with_capacity(owner_capacity),
            count: Vec::with_capacity(owner_capacity),
            lookup: vec![SendLookup::EMPTY; max_sends],
        }
    }

    /// オーナーが spawn したとき空 row を 1 個 push する。
    pub(crate) fn push_empty_row(&mut self) {
        self.dest_dense.push([0; CAP]);
        self.gain.push([0.0; CAP]);
        self.position.push([0; CAP]);
        self.dest_kind.push([0; CAP]);
        self.id.push([SendId::INVALID; CAP]);
        self.count.push(0);
    }

    /// オーナーの spawn 失敗時に push 済み row を巻き戻す。
    pub(crate) fn pop_row(&mut self) {
        self.dest_dense.pop();
        self.gain.pop();
        self.position.pop();
        self.dest_kind.pop();
        self.id.pop();
        self.count.pop();
    }

    /// オーナー despawn 時の row 単位 swap_remove。
    /// 1) 当該 row から出ていた送信元 lookup をクリア
    /// 2) SoA 全列を swap_remove
    /// 3) 末尾から移動してきた row が指していた lookup の `owner_dense` を更新
    pub(crate) fn swap_remove_row(&mut self, dense_index: usize) {
        let last_dense = self.count.len() - 1;
        let dense_u32 = dense_index as u32;
        let last_u32 = last_dense as u32;

        // 1. lookup クリア (本体は後で swap_remove)。
        let n = self.count[dense_index] as usize;
        for slot in 0..n {
            let sid = self.id[dense_index][slot];
            if sid.is_valid() && (sid.index as usize) < self.lookup.len() {
                let lk = &self.lookup[sid.index as usize];
                if lk.generation == sid.generation {
                    self.lookup[sid.index as usize] = SendLookup::EMPTY;
                }
            }
        }

        // 2. SoA swap_remove。
        self.dest_dense.swap_remove(dense_index);
        self.gain.swap_remove(dense_index);
        self.position.swap_remove(dense_index);
        self.dest_kind.swap_remove(dense_index);
        self.id.swap_remove(dense_index);
        self.count.swap_remove(dense_index);

        // 3. 末尾 row が dense_index に移動したので lookup を更新。
        if dense_index != last_dense && dense_index < self.count.len() {
            let n = self.count[dense_index] as usize;
            for slot in 0..n {
                let sid = self.id[dense_index][slot];
                if sid.is_valid() && (sid.index as usize) < self.lookup.len() {
                    let lk = &mut self.lookup[sid.index as usize];
                    if lk.owner_dense == last_u32 && lk.generation == sid.generation {
                        lk.owner_dense = dense_u32;
                    }
                }
            }
        }
        let _ = dense_u32;
    }

    /// 指定 owner の chain 末尾に Send を追加する。chain 満杯 / `owner_dense` 範囲外 /
    /// `id.index` が `lookup` 範囲外なら `false`。
    pub(crate) fn add_send(
        &mut self,
        owner_dense: usize,
        id: SendId,
        dest_dense: u32,
        dest_kind: u8,
        gain: f32,
        position: u8,
    ) -> bool {
        if owner_dense >= self.count.len() {
            return false;
        }
        let count = self.count[owner_dense] as usize;
        if count >= CAP {
            return false;
        }
        if (id.index as usize) >= self.lookup.len() {
            return false;
        }
        self.dest_dense[owner_dense][count] = dest_dense;
        self.gain[owner_dense][count] = gain;
        self.position[owner_dense][count] = position;
        self.dest_kind[owner_dense][count] = dest_kind;
        self.id[owner_dense][count] = id;
        self.count[owner_dense] = (count + 1) as u8;
        self.lookup[id.index as usize] = SendLookup {
            owner_dense: owner_dense as u32,
            slot: count as u8,
            generation: id.generation,
        };
        true
    }

    /// `SendId` で Send を削除する。stale (generation 不一致) または未存在で `false`。
    pub(crate) fn remove_by_id(&mut self, id: SendId) -> bool {
        let Some((owner_dense, slot)) = self.resolve(id) else {
            return false;
        };
        self.swap_remove_slot(owner_dense, slot);
        true
    }

    /// `SendId` の現在位置 `(owner_dense, slot)` を返す。stale なら `None`。
    pub(crate) fn resolve(&self, id: SendId) -> Option<(usize, usize)> {
        if !id.is_valid() {
            return None;
        }
        let lk = self.lookup.get(id.index as usize)?;
        if lk.is_empty() || lk.generation != id.generation {
            return None;
        }
        Some((lk.owner_dense as usize, lk.slot as usize))
    }

    pub(crate) fn set_gain(&mut self, id: SendId, gain: f32) -> bool {
        if let Some((owner_dense, slot)) = self.resolve(id) {
            self.gain[owner_dense][slot] = gain;
            true
        } else {
            false
        }
    }

    pub(crate) fn set_position(&mut self, id: SendId, position: u8) -> bool {
        if let Some((owner_dense, slot)) = self.resolve(id) {
            self.position[owner_dense][slot] = position;
            true
        } else {
            false
        }
    }

    /// dense index 指定で gain を直接書き込む (Snapshot 補間で使用)。
    #[inline]
    pub(crate) fn write_gain_by_dense(&mut self, owner_dense: usize, slot: usize, gain: f32) {
        if let Some(arr) = self.gain.get_mut(owner_dense)
            && slot < CAP
        {
            arr[slot] = gain;
        }
    }

    /// dense index 指定で gain を読み出す (Snapshot apply 時の現在値キャプチャ用)。
    #[inline]
    pub(crate) fn gain_at(&self, owner_dense: usize, slot: usize) -> Option<f32> {
        self.gain
            .get(owner_dense)
            .and_then(|arr| arr.get(slot))
            .copied()
    }

    /// owner `owner_dense` の Send 数。
    #[inline]
    pub(crate) fn count_at(&self, owner_dense: usize) -> usize {
        self.count
            .get(owner_dense)
            .copied()
            .map(|c| c as usize)
            .unwrap_or(0)
    }

    /// owner `owner_dense` の slot `slot` にある Send 情報 `(dest_dense, gain, position, dest_kind)`。
    #[inline]
    pub(crate) fn send_at(&self, owner_dense: usize, slot: usize) -> (u32, f32, u8, u8) {
        (
            self.dest_dense[owner_dense][slot],
            self.gain[owner_dense][slot],
            self.position[owner_dense][slot],
            self.dest_kind[owner_dense][slot],
        )
    }

    /// 内部用: owner `owner_dense` の slot `slot` にある Send を swap-remove する。
    /// `lookup` の整合も維持する (除去された Send をクリア、移動された Send の slot を更新)。
    pub(crate) fn swap_remove_slot(&mut self, owner_dense: usize, slot: usize) {
        let count = self.count[owner_dense] as usize;
        debug_assert!(slot < count);

        let removed_sid = self.id[owner_dense][slot];
        if removed_sid.is_valid() && (removed_sid.index as usize) < self.lookup.len() {
            let lk = &self.lookup[removed_sid.index as usize];
            if lk.generation == removed_sid.generation {
                self.lookup[removed_sid.index as usize] = SendLookup::EMPTY;
            }
        }

        let last = count - 1;
        if slot != last {
            self.dest_dense[owner_dense][slot] = self.dest_dense[owner_dense][last];
            self.gain[owner_dense][slot] = self.gain[owner_dense][last];
            self.position[owner_dense][slot] = self.position[owner_dense][last];
            self.dest_kind[owner_dense][slot] = self.dest_kind[owner_dense][last];
            self.id[owner_dense][slot] = self.id[owner_dense][last];

            let moved_sid = self.id[owner_dense][slot];
            if moved_sid.is_valid() && (moved_sid.index as usize) < self.lookup.len() {
                let lk = &mut self.lookup[moved_sid.index as usize];
                if lk.owner_dense == owner_dense as u32 && lk.generation == moved_sid.generation {
                    lk.slot = slot as u8;
                }
            }
        }
        self.count[owner_dense] -= 1;
    }

    /// バス despawn 時の cross-owner cleanup。
    /// `target_dense` を宛先とする slot を全 row から一括除去する。
    /// `exclude_owner` は除外 (本人 row は別途 `swap_remove_row` で処理されるため)。
    pub(crate) fn remove_destinations_matching(
        &mut self,
        target_dense: u32,
        exclude_owner: usize,
    ) {
        for src_dense in 0..self.count.len() {
            if src_dense == exclude_owner {
                continue;
            }
            let mut s = 0;
            while s < self.count[src_dense] as usize {
                if self.dest_dense[src_dense][s] == target_dense {
                    self.swap_remove_slot(src_dense, s);
                    // s はそのまま (末尾要素が入った)。
                } else {
                    s += 1;
                }
            }
        }
    }

    /// バス despawn (swap_remove_row 後) の dest_dense 再マップ。
    /// `removed_dense` を指していた slot は `fallback` に、
    /// `last_dense_was` を指していた slot は `removed_dense` に書き換える
    /// (swap_remove で末尾 → removed_dense に移動した分の追従)。
    pub(crate) fn remap_destinations_after_swap(
        &mut self,
        removed_dense: u32,
        last_dense_was: u32,
        fallback: u32,
    ) {
        let needs_last_remap = last_dense_was != removed_dense;
        for d in 0..self.count.len() {
            let n = self.count[d] as usize;
            for s in 0..n {
                let dest = self.dest_dense[d][s];
                if dest == removed_dense {
                    self.dest_dense[d][s] = fallback;
                } else if needs_last_remap && dest == last_dense_was {
                    self.dest_dense[d][s] = removed_dense;
                }
            }
        }
    }
}
