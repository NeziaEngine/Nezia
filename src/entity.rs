/// 物理ID（Entity ID）。
///
/// スパースセットが内部で発行する `(index, generation)` の組。
/// `index` は密配列への O(1) アクセスに使用し、
/// `generation` はスロット再利用時の無効化検出に使う。
///
/// ## C# で言うと
///
/// ```csharp
/// // C# なら GCHandle や WeakReference で参照の有効性を追跡するが、
/// // Rust にはGCが無いので generation カウンタで手動管理する。
/// struct EntityId {
///     public uint Index;      // 配列の添字（= List<T> のインデックス）
///     public uint Generation; // スロット再利用を検出するバージョン番号
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntityId {
    pub index: u32,
    pub generation: u32,
}
