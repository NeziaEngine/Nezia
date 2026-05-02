use std::collections::{HashMap, VecDeque};

use crate::bus::MAX_BUSES;
use crate::entity::EntityId;

/// メインスレッドが保持するバスルーティングのミラー。
///
/// ループ検出・トポロジカルソートをサウンドスレッドに依存せず行うために、
/// メインスレッド側でバス接続情報を複製保持する。
pub struct BusRoutingMirror {
    /// マスターバスの EntityId。
    pub master_bus_id: EntityId,
    /// 次に発行する entity_index（単調増加）。
    pub next_index: u32,
    /// bus entity_index → 親バスの entity_index。マスターバスは自己参照。
    routing: Vec<Option<u32>>,
    /// bus entity_index → 密配列インデックス。
    entity_to_dense: Vec<Option<u32>>,
    /// 現在有効なバスの entity_index リスト。
    entity_indices: Vec<u32>,
}

impl BusRoutingMirror {
    /// マスターバス（entity_index=0, dense=0）で初期化する。
    pub fn new(master_id: EntityId) -> Self {
        let master_idx = master_id.index as usize;
        let mut routing = vec![None; master_idx + 1];
        let mut entity_to_dense = vec![None; master_idx + 1];
        routing[master_idx] = Some(master_idx as u32); // 自己参照
        entity_to_dense[master_idx] = Some(0u32);

        Self {
            master_bus_id: master_id,
            next_index: master_id.index + 1,
            routing,
            entity_to_dense,
            entity_indices: vec![master_id.index],
        }
    }

    /// バスの EntityId を密配列インデックスに解決する。
    pub fn resolve_dense(&self, id: EntityId) -> Option<u32> {
        self.entity_to_dense
            .get(id.index as usize)?
            .as_ref()
            .copied()
    }

    /// 現在有効なバス数を返す。
    pub fn len(&self) -> usize {
        self.entity_indices.len()
    }

    /// `index` が収まるようにルーティング配列を拡張する。
    pub fn ensure_capacity(&mut self, index: usize) {
        if index >= self.routing.len() {
            self.routing.resize(index + 1, None);
            self.entity_to_dense.resize(index + 1, None);
        }
    }

    /// バスを追加する。
    ///
    /// - `entity_index`: 発行済みの新規 entity_index
    /// - `parent_entity_index`: 親バスの entity_index
    /// - `dense`: 割り当てる密配列インデックス
    pub fn insert(&mut self, entity_index: u32, parent_entity_index: u32, dense: u32) {
        self.ensure_capacity(entity_index as usize);
        self.routing[entity_index as usize] = Some(parent_entity_index);
        self.entity_to_dense[entity_index as usize] = Some(dense);
        self.entity_indices.push(entity_index);
    }

    /// バスを削除する。
    pub fn remove(&mut self, entity_index: u32) {
        let idx = entity_index as usize;
        if idx < self.routing.len() {
            self.routing[idx] = None;
            self.entity_to_dense[idx] = None;
        }
        self.entity_indices.retain(|&i| i != entity_index);
    }

    /// 親バスを変更する。
    pub fn set_parent(&mut self, entity_index: u32, parent_entity_index: u32) {
        if let Some(slot) = self.routing.get_mut(entity_index as usize) {
            *slot = Some(parent_entity_index);
        }
    }

    /// `start` から辿って `target` に到達するか確認する（ループ検出）。
    pub fn has_loop(&self, start: u32, target: u32) -> bool {
        let mut current = target;
        let master_idx = self.master_bus_id.index;
        for _ in 0..MAX_BUSES {
            if current == start {
                return true;
            }
            if current == master_idx {
                return false;
            }
            match self.routing.get(current as usize).and_then(|r| *r) {
                Some(parent) => current = parent,
                None => return false,
            }
        }
        false
    }

    /// トポロジカルソート（リーフ→ルート）を計算し、密配列インデックス列で返す。
    pub fn compute_process_order(&self) -> Vec<u32> {
        let master_idx = self.master_bus_id.index;

        // in_degree: 何個の子バスがこのバスを親として参照しているか。
        let mut in_degree: HashMap<u32, usize> = HashMap::new();
        for &entity_idx in &self.entity_indices {
            in_degree.entry(entity_idx).or_insert(0);
        }
        for &entity_idx in &self.entity_indices {
            if entity_idx == master_idx {
                continue;
            }
            let parent = self
                .routing
                .get(entity_idx as usize)
                .and_then(|r| *r)
                .unwrap_or(master_idx);
            *in_degree.entry(parent).or_insert(0) += 1;
        }

        // リーフ（in_degree == 0）からキューに入れる。
        let mut queue: VecDeque<u32> = self
            .entity_indices
            .iter()
            .copied()
            .filter(|&i| in_degree.get(&i).copied().unwrap_or(0) == 0)
            .collect();

        let mut order = Vec::with_capacity(self.entity_indices.len());

        while let Some(entity_idx) = queue.pop_front() {
            // entity_index → 密配列インデックスに変換。
            if let Some(dense) = self
                .entity_to_dense
                .get(entity_idx as usize)
                .and_then(|d| *d)
            {
                order.push(dense);
            }

            if entity_idx == master_idx {
                continue;
            }
            let parent = self
                .routing
                .get(entity_idx as usize)
                .and_then(|r| *r)
                .unwrap_or(master_idx);
            if let Some(deg) = in_degree.get_mut(&parent) {
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(parent);
                }
            }
        }

        order
    }
}
