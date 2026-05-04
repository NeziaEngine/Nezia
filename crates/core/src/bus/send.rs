//! Send (副ルート) 関連の型定義。
//!
//! Send は「バスから別バスへの追加経路 (本線とは別)」を表す。`SendId` は二層 ID
//! (`index`, `generation`) で、メインスレッドが事前発行しサウンドスレッドが
//! 受け取って `BusWorld` 内 SoA に書き込む。
//!
//! Phase 3-3 PR1 では宛先がバスのみ。Compressor sidechain への Send は PR2 で追加する。

/// Send 識別ハンドル (二層 ID)。
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct SendId {
    pub index: u32,
    pub generation: u32,
}

impl SendId {
    /// 無効 SendId (空 slot を表す番兵)。
    pub const INVALID: SendId = SendId {
        index: u32::MAX,
        generation: 0,
    };

    pub fn is_valid(&self) -> bool {
        self.index != u32::MAX
    }
}

/// Send のタップ位置。
///
/// - `Pre`: Pre-Fader chain 適用後、Fader (gain/mute) 適用前で tap。
///   本線 mute / gain 0.0 でも Send は流れる (sidechain trigger 用途で重要)。
/// - `Post`: Post-Fader chain 適用後、親バスへの加算直前で tap。
///   本線 mute なら Send もゼロ (一般的な Aux Reverb 用途)。
#[repr(u8)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SendPosition {
    Pre = 0,
    Post = 1,
}

impl SendPosition {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(SendPosition::Pre),
            1 => Some(SendPosition::Post),
            _ => None,
        }
    }
}
