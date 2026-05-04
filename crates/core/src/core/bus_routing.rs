use std::collections::{HashMap, HashSet, VecDeque};

use crate::bus::{MAX_BUSES, MAX_SENDS, SendId};
use crate::entity::EntityId;

/// バス間 Send エッジ (メインスレッドミラー用)。
#[derive(Copy, Clone, Debug)]
pub(crate) struct SendEdge {
    pub send_id: SendId,
    /// Send 元バスの entity_index。
    pub src_entity: u32,
    /// Send 宛先バスの entity_index。
    pub dst_entity: u32,
}

/// メインスレッドが保持するバスルーティングのミラー。
///
/// ループ検出・トポロジカルソートをサウンドスレッドに依存せず行うために、
/// メインスレッド側でバス接続情報を複製保持する。
/// Phase 3-3 で Send (副ルート) を加え、グラフは木 → DAG に拡張された。
pub struct BusRoutingMirror {
    /// マスターバスの EntityId。
    pub master_bus_id: EntityId,
    /// 次に発行する entity_index（単調増加）。
    pub next_index: u32,
    /// bus entity_index → 親バスの entity_index。マスターバスは自己参照。
    routing: Vec<Option<u32>>,
    /// bus entity_index → 密配列インデックス。
    /// `BusWorld` 側の swap_remove と同期するため `remove` 時に更新される。
    entity_to_dense: Vec<Option<u32>>,
    /// dense_index → entity_index。`BusWorld` の dense 配列と同形式。
    /// 末尾要素を swap_remove で詰めるため両方向のマッピングを保つ必要がある。
    dense_to_entity: Vec<u32>,
    /// 現在有効な Send エッジ。`SendId.index` がランダムアクセスのキー。
    /// 削除時は当該エントリを `None` にする。
    sends: Vec<Option<SendEdge>>,
}

#[allow(dead_code)]
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
            dense_to_entity: vec![master_id.index],
            sends: vec![None; MAX_SENDS],
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
        self.dense_to_entity.len()
    }

    /// `index` が収まるようにルーティング配列を拡張する。
    pub fn ensure_capacity(&mut self, index: usize) {
        if index >= self.routing.len() {
            self.routing.resize(index + 1, None);
            self.entity_to_dense.resize(index + 1, None);
        }
    }

    /// バスを追加する。
    pub fn insert(&mut self, entity_index: u32, parent_entity_index: u32, dense: u32) {
        self.ensure_capacity(entity_index as usize);
        self.routing[entity_index as usize] = Some(parent_entity_index);
        self.entity_to_dense[entity_index as usize] = Some(dense);
        // dense_to_entity は単調 push (insert 時の dense は常に末尾)。
        debug_assert_eq!(dense as usize, self.dense_to_entity.len());
        self.dense_to_entity.push(entity_index);
    }

    /// バスを削除する。
    ///
    /// `BusWorld` の swap_remove と整合させるため、最後尾バスの dense_index を
    /// 削除位置に詰め直す。同時に当該バスを `src` または `dst` とする Send も削除する。
    /// 戻り値: 削除されたバスを `src` / `dst` としていた SendId のリスト
    /// (呼出側で `SendIdAllocator::free` する)。
    pub fn remove(&mut self, entity_index: u32) -> Vec<SendId> {
        let idx = entity_index as usize;
        let Some(dense_index) = self.entity_to_dense.get(idx).and_then(|d| *d) else {
            return Vec::new();
        };

        // 1. このバスを src または dst とする Send を抽出して除去。
        let mut freed = Vec::new();
        for slot in self.sends.iter_mut() {
            if let Some(edge) = *slot
                && (edge.src_entity == entity_index || edge.dst_entity == entity_index)
            {
                freed.push(edge.send_id);
                *slot = None;
            }
        }

        // 2. dense_to_entity の swap_remove。最後尾を dense_index に詰める。
        let last_dense = self.dense_to_entity.len() - 1;
        if dense_index as usize != last_dense {
            let moved_entity = self.dense_to_entity[last_dense];
            self.dense_to_entity[dense_index as usize] = moved_entity;
            self.entity_to_dense[moved_entity as usize] = Some(dense_index);
        }
        self.dense_to_entity.pop();

        // 3. 削除対象自体のマッピングをクリア。
        self.routing[idx] = None;
        self.entity_to_dense[idx] = None;

        freed
    }

    /// 親バスを変更する。
    pub fn set_parent(&mut self, entity_index: u32, parent_entity_index: u32) {
        if let Some(slot) = self.routing.get_mut(entity_index as usize) {
            *slot = Some(parent_entity_index);
        }
    }

    /// 本線のループ検出 (`set_bus_output` 用)。
    /// `start` から本線を辿って `target` に到達するか確認する。
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

    // ── Send 操作 (Phase 3-3) ──

    /// 既存 Send + これから追加する `(src → dst)` で循環が生じるかチェックする。
    /// `dst` から本線 + 既存 Send を辿って `src` に到達できれば循環。
    pub fn would_create_send_cycle(&self, src: u32, dst: u32) -> bool {
        if src == dst {
            return true;
        }
        let master_idx = self.master_bus_id.index;
        let mut stack = vec![dst];
        let mut visited: HashSet<u32> = HashSet::new();

        // 隣接リスト: entity_index → そこから出ていく宛先 entity_index 群。
        let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
        for &eidx in &self.dense_to_entity {
            // 本線エッジ (master 以外)
            if eidx != master_idx
                && let Some(parent) = self.routing.get(eidx as usize).and_then(|r| *r)
            {
                adj.entry(eidx).or_default().push(parent);
            }
        }
        for edge in self.sends.iter().flatten() {
            adj.entry(edge.src_entity)
                .or_default()
                .push(edge.dst_entity);
        }

        while let Some(cur) = stack.pop() {
            if cur == src {
                return true;
            }
            if !visited.insert(cur) {
                continue;
            }
            if let Some(neighbors) = adj.get(&cur) {
                for &n in neighbors {
                    stack.push(n);
                }
            }
        }
        false
    }

    /// Send エッジを登録する。`SendId.index` が範囲外なら `false`。
    pub fn add_send(&mut self, edge: SendEdge) -> bool {
        let i = edge.send_id.index as usize;
        if i >= self.sends.len() {
            return false;
        }
        self.sends[i] = Some(edge);
        true
    }

    /// Send エッジを削除する。SendId が未存在なら `false`。
    pub fn remove_send(&mut self, send_id: SendId) -> bool {
        let i = send_id.index as usize;
        if i >= self.sends.len() {
            return false;
        }
        let existed = self.sends[i]
            .filter(|e| e.send_id.generation == send_id.generation)
            .is_some();
        if existed {
            self.sends[i] = None;
        }
        existed
    }

    /// SendId からエッジを取得する。stale なら `None`。
    pub fn send(&self, send_id: SendId) -> Option<&SendEdge> {
        let i = send_id.index as usize;
        let edge = self.sends.get(i)?.as_ref()?;
        if edge.send_id.generation == send_id.generation {
            Some(edge)
        } else {
            None
        }
    }

    /// DAG トポロジカルソート (入力先行順) を計算し、密配列インデックス列で返す。
    ///
    /// 木構造の「リーフ→ルート」と異なり、Send 経路を含む DAG では
    /// 「全入力 (本線 + Send) が先行するノードを順に処理する」順序を計算する。
    /// 内部にサイクルがある場合 (本来 add 時に弾かれるが防御的) は、
    /// 処理可能だった分だけを返す。
    pub fn compute_process_order(&self) -> Vec<u32> {
        let master_idx = self.master_bus_id.index;

        // in_degree: 何個のエッジ (本線 + Send) がこのバスを宛先としているか。
        let mut in_degree: HashMap<u32, usize> = HashMap::new();
        for &eidx in &self.dense_to_entity {
            in_degree.insert(eidx, 0);
        }
        // 本線エッジ: child → parent
        for &eidx in &self.dense_to_entity {
            if eidx == master_idx {
                continue;
            }
            let parent = self
                .routing
                .get(eidx as usize)
                .and_then(|r| *r)
                .unwrap_or(master_idx);
            *in_degree.entry(parent).or_insert(0) += 1;
        }
        // Send エッジ: src → dst
        for edge in self.sends.iter().flatten() {
            *in_degree.entry(edge.dst_entity).or_insert(0) += 1;
        }

        // in_degree == 0 のバスからキューに入れる。
        let mut queue: VecDeque<u32> = self
            .dense_to_entity
            .iter()
            .copied()
            .filter(|&i| in_degree.get(&i).copied().unwrap_or(0) == 0)
            .collect();

        let mut order = Vec::with_capacity(self.dense_to_entity.len());

        while let Some(entity_idx) = queue.pop_front() {
            if let Some(dense) = self
                .entity_to_dense
                .get(entity_idx as usize)
                .and_then(|d| *d)
            {
                order.push(dense);
            }

            // 本線エッジを切る。
            if entity_idx != master_idx {
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
            // Send エッジを切る。
            for edge in self.sends.iter().flatten() {
                if edge.src_entity == entity_idx
                    && let Some(deg) = in_degree.get_mut(&edge.dst_entity)
                {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(edge.dst_entity);
                    }
                }
            }
        }

        order
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn master_id() -> EntityId {
        EntityId {
            index: 0,
            generation: 0,
        }
    }

    #[test]
    fn remove_compacts_dense_indices() {
        // 既存の latent bug の回帰テスト: middle bus を削除したとき、後続バスの
        // dense index が swap_remove で詰められることを確認する。
        let mut m = BusRoutingMirror::new(master_id());
        m.insert(1, 0, 1);
        m.insert(2, 0, 2);
        m.insert(3, 0, 3);
        assert_eq!(
            m.resolve_dense(EntityId {
                index: 1,
                generation: 0
            }),
            Some(1)
        );
        assert_eq!(
            m.resolve_dense(EntityId {
                index: 3,
                generation: 0
            }),
            Some(3)
        );

        m.remove(1);
        // Bus 3 が dense=1 に詰められる (last → removed slot)。
        assert_eq!(
            m.resolve_dense(EntityId {
                index: 3,
                generation: 0
            }),
            Some(1)
        );
        assert_eq!(
            m.resolve_dense(EntityId {
                index: 2,
                generation: 0
            }),
            Some(2)
        );
        assert_eq!(
            m.resolve_dense(EntityId {
                index: 1,
                generation: 0
            }),
            None
        );
    }

    #[test]
    fn would_create_send_cycle_simple() {
        let mut m = BusRoutingMirror::new(master_id());
        m.insert(1, 0, 1);
        m.insert(2, 0, 2);

        // 同一バス自身への Send は循環。
        assert!(m.would_create_send_cycle(1, 1));
        // 1 → 2 はサイクルなし。
        assert!(!m.would_create_send_cycle(1, 2));

        m.add_send(SendEdge {
            send_id: SendId {
                index: 0,
                generation: 0,
            },
            src_entity: 1,
            dst_entity: 2,
        });
        // 2 → 1 (既に 1 → 2 がある) はサイクル。
        assert!(m.would_create_send_cycle(2, 1));
    }

    #[test]
    fn would_create_send_cycle_via_primary() {
        // 本線の親子関係も DAG 探索の対象。
        let mut m = BusRoutingMirror::new(master_id());
        m.insert(1, 0, 1); // 1 → master
        m.insert(2, 1, 2); // 2 → 1
        // master からみると 2 → 1 → 0 という本線経路がある。
        // master が src で 2 が dst の Send を貼ると 2 → 1 → 0 (master) → 2 で循環。
        assert!(m.would_create_send_cycle(0, 2));
    }

    #[test]
    fn topo_order_with_send() {
        // BGM (idx=1) → Aux (idx=2) Send。Aux は master 直結。
        let mut m = BusRoutingMirror::new(master_id());
        m.insert(1, 0, 1); // BGM, parent=master
        m.insert(2, 0, 2); // Aux, parent=master
        m.add_send(SendEdge {
            send_id: SendId {
                index: 0,
                generation: 0,
            },
            src_entity: 1,
            dst_entity: 2,
        });
        let order = m.compute_process_order();
        // BGM (no input) → Aux (BGM Send) → Master (本線 BGM + 本線 Aux)。
        assert_eq!(order.len(), 3);
        let pos_bgm = order.iter().position(|&d| d == 1).unwrap();
        let pos_aux = order.iter().position(|&d| d == 2).unwrap();
        let pos_master = order.iter().position(|&d| d == 0).unwrap();
        assert!(pos_bgm < pos_aux, "BGM should be processed before Aux");
        assert!(
            pos_aux < pos_master,
            "Aux should be processed before master"
        );
    }

    #[test]
    fn remove_returns_freed_send_ids() {
        let mut m = BusRoutingMirror::new(master_id());
        m.insert(1, 0, 1);
        m.insert(2, 0, 2);
        let sid_a = SendId {
            index: 0,
            generation: 0,
        };
        let sid_b = SendId {
            index: 1,
            generation: 0,
        };
        m.add_send(SendEdge {
            send_id: sid_a,
            src_entity: 1,
            dst_entity: 2,
        });
        m.add_send(SendEdge {
            send_id: sid_b,
            src_entity: 2,
            dst_entity: 1,
        });

        // Bus 1 を消すと両方の Send が解放される。
        let freed = m.remove(1);
        assert_eq!(freed.len(), 2);
        assert!(freed.contains(&sid_a));
        assert!(freed.contains(&sid_b));
    }
}
