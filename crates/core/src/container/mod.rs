//! Container (Random / Switch / Sequence) — メインスレッド完結型の再生指示解決層。
//!
//! 設計詳細は [docs/design/core/container.md](../../../../docs/design/core/container.md) を参照。
//! Phase 4-2 第一弾では Random Container のみ実装する。
//!
//! audio thread には流れない (新規 Command も新規 World も追加しない)。
//! `play_container()` がメインスレッドで子を 1 つ選び、既存の Source 再生 API に委譲する。

mod world;

pub(crate) use world::{ContainerWorld, RandomPick};

use crate::buffer_pool::BufferId;

/// Container を識別するハンドル (メインスレッド側のみ)。
///
/// `EntityId` とは別の型。Container は audio thread に流れないため、
/// 二層 ID 設計 (CLAUDE.md 参照) における物理 ID とは意味が異なる。
///
/// `#[repr(C)]` は将来の FFI 層 (`NeziaContainerId`) とのゼロコピー受け渡し用。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContainerId {
    pub index: u32,
    pub generation: u32,
}

/// Container の子要素。
///
/// Phase 4-2 第一弾では `Source(BufferId)` のみ受け付ける。
/// 将来のネスト対応時に `Container(ContainerId)` バリアントを追加する想定で
/// 最初から enum 化しておく (破壊的変更を回避)。
#[derive(Debug, Clone, Copy)]
pub(crate) enum ContainerChild {
    Source(BufferId),
    // Container(ContainerId), // 将来用
}

/// Container ワールドの最大スロット数。
pub const MAX_CONTAINERS: usize = 128;
