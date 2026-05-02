/// 物理ID（Entity ID）。
///
/// スパースセットが内部で発行する `(index, generation)` の組。
/// `index` は密配列への O(1) アクセスに使用し、
/// `generation` はスロット再利用時の無効化検出に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntityId {
    pub index: u32,
    pub generation: u32,
}
