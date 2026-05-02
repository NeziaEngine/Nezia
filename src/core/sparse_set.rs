use crate::entity::EntityId;

/// 汎用スパースセット。
///
/// エンティティのスロット管理（割り当て・解放・世代検証）を一元化する。
/// コンポーネントデータ（密配列）は所有しない。各 World が独自に保持し、
/// `SparseSet` が返すインデックスで同期的に `push` / `swap_remove` する。
pub struct SparseSet {
    /// EntityId.index -> 密配列インデックスへのマッピング。
    sparse: Vec<Option<SparseEntry>>,
    /// 密配列インデックス -> EntityId.index への逆マッピング。
    dense_to_sparse: Vec<u32>,
    /// 解放済みスロットの再利用リスト。
    free_list: Vec<u32>,
    /// 次に発行するインデックス（単調増加）。
    next_index: u32,
    /// 最大エンティティ数。
    capacity: usize,
}

#[derive(Debug, Clone, Copy)]
struct SparseEntry {
    dense_index: u32,
    generation: u32,
}

impl SparseSet {
    /// 指定キャパシティでスパースセットを生成する。
    pub fn new(capacity: usize) -> Self {
        Self {
            sparse: Vec::with_capacity(capacity),
            dense_to_sparse: Vec::with_capacity(capacity),
            free_list: Vec::with_capacity(capacity),
            next_index: 0,
            capacity,
        }
    }

    /// 現在のエンティティ数（密配列の長さ）。
    pub fn len(&self) -> usize {
        self.dense_to_sparse.len()
    }

    pub fn is_empty(&self) -> bool {
        self.dense_to_sparse.is_empty()
    }

    /// EntityId を検証し、有効なら密配列インデックスを返す。
    pub fn resolve(&self, id: EntityId) -> Option<usize> {
        let entry = self.sparse.get(id.index as usize)?.as_ref()?;
        if entry.generation != id.generation {
            return None;
        }
        Some(entry.dense_index as usize)
    }

    /// EntityId が有効か確認する。
    pub fn contains(&self, id: EntityId) -> bool {
        self.resolve(id).is_some()
    }

    /// スロットを割り当てる（fire-and-forget 用）。
    ///
    /// 成功時は `(EntityId, dense_index)` を返す。
    /// 呼び出し側はこの `dense_index` に合わせて全コンポーネント配列に `push` すること。
    pub fn alloc(&mut self) -> Option<(EntityId, usize)> {
        if self.len() >= self.capacity {
            return None;
        }
        let dense_index = self.len();

        let (index, generation) = if let Some(reused) = self.free_list.pop() {
            let reused_gen = self.sparse[reused as usize]
                .map(|e| e.generation)
                .unwrap_or(0);
            self.sparse[reused as usize] = Some(SparseEntry {
                dense_index: dense_index as u32,
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
                dense_index: dense_index as u32,
                generation: 0,
            });
            (index, 0)
        };

        self.dense_to_sparse.push(index);
        Some((EntityId { index, generation }, dense_index))
    }

    /// 事前割り当てされた EntityId でスロットを確保する。
    ///
    /// 同じ index が既に使用中の場合は `None` を返す。
    /// 成功時は密配列インデックスを返す。
    /// 呼び出し側はこのインデックスに合わせて全コンポーネント配列に `push` すること。
    pub fn alloc_with_id(&mut self, id: EntityId) -> Option<usize> {
        if self.len() >= self.capacity {
            return None;
        }
        if id.index as usize >= self.sparse.len() {
            self.sparse.resize(id.index as usize + 1, None);
        }
        if self.sparse[id.index as usize].is_some() {
            return None;
        }

        let dense_index = self.len();
        self.sparse[id.index as usize] = Some(SparseEntry {
            dense_index: dense_index as u32,
            generation: id.generation,
        });
        self.dense_to_sparse.push(id.index);

        if id.index >= self.next_index {
            self.next_index = id.index + 1;
        }

        Some(dense_index)
    }

    /// 指定した entity_index でスロットを確保する（内部用）。
    ///
    /// `alloc_with_id` と異なり、解放済みスロットの再利用を許可する。
    /// generation は既存エントリから引き継ぐ。
    /// 成功時は `(EntityId, dense_index)` を返す。
    pub fn alloc_at_index(&mut self, entity_index: u32) -> Option<(EntityId, usize)> {
        if self.len() >= self.capacity {
            return None;
        }
        if entity_index as usize >= self.sparse.len() {
            self.sparse.resize(entity_index as usize + 1, None);
        }

        let dense_index = self.len();
        let generation = self.sparse[entity_index as usize]
            .map(|e| e.generation)
            .unwrap_or(0);

        self.sparse[entity_index as usize] = Some(SparseEntry {
            dense_index: dense_index as u32,
            generation,
        });

        self.free_list.retain(|&i| i != entity_index);

        if entity_index >= self.next_index {
            self.next_index = entity_index + 1;
        }

        self.dense_to_sparse.push(entity_index);
        let id = EntityId {
            index: entity_index,
            generation,
        };
        Some((id, dense_index))
    }

    /// EntityId でスロットを解放する（swap-remove）。
    ///
    /// 成功時は解放された密配列インデックスを返す。
    /// 呼び出し側は全コンポーネント配列で `swap_remove(dense_index)` すること。
    pub fn dealloc(&mut self, id: EntityId) -> Option<usize> {
        let dense_index = self.resolve(id)?;
        self.dealloc_inner(dense_index, id.index);
        Some(dense_index)
    }

    /// 密配列インデックスでスロットを解放する（swap-remove）。
    ///
    /// 逆順で呼び出すこと（swap-remove のため後ろから消さないとインデックスがずれる）。
    /// 呼び出し側は全コンポーネント配列で `swap_remove(dense_index)` すること。
    pub fn dealloc_by_dense_index(&mut self, dense_index: usize) -> bool {
        if dense_index >= self.len() {
            return false;
        }
        let sparse_index = self.dense_to_sparse[dense_index];
        self.dealloc_inner(dense_index, sparse_index);
        true
    }

    fn dealloc_inner(&mut self, dense_index: usize, sparse_index: u32) {
        if let Some(entry) = &mut self.sparse[sparse_index as usize] {
            *entry = SparseEntry {
                dense_index: 0,
                generation: entry.generation + 1,
            };
        }
        self.free_list.push(sparse_index);

        let last_dense = self.len() - 1;
        if dense_index != last_dense {
            let moved_sparse_index = self.dense_to_sparse[last_dense];
            if let Some(entry) = &mut self.sparse[moved_sparse_index as usize] {
                entry.dense_index = dense_index as u32;
            }
            self.dense_to_sparse[dense_index] = moved_sparse_index;
        }
        self.dense_to_sparse.swap_remove(dense_index);
    }
}
