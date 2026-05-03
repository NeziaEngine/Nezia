/// 物理ID（Entity ID）。
///
/// スパースセットが内部で発行する `(index, generation)` の組。
/// `index` は密配列への O(1) アクセスに使用し、
/// `generation` はスロット再利用時の無効化検出に使う。
///
/// `#[repr(C)]` は FFI 層 (`NeziaEntityId`) との **ゼロコピー受け渡し**を成立させるための
/// レイアウト保証。フィールド並びを変えるとバインディングと ABI 不整合になる。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntityId {
    pub index: u32,
    pub generation: u32,
}

/// `batch_set_source_positions()` の入力要素。
///
/// `#[repr(C)]` で固定レイアウトとし、FFI 層の `NeziaSourcePositionUpdate` と
/// バイト並びを一致させる。これによりバインディング側から受け取った配列を
/// 変換コピーなしでそのまま渡せる。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourcePositionUpdate {
    pub source: EntityId,
    pub position: [f32; 3],
}

/// SP-10: `batch_set_source_velocities()` の入力要素。
///
/// `#[repr(C)]` で固定レイアウトとし、FFI 層の `NeziaSourceVelocityUpdate` と
/// バイト並びを一致させる。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceVelocityUpdate {
    pub source: EntityId,
    pub velocity: [f32; 3],
}
