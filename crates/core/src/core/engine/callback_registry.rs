use std::collections::HashMap;

/// 再生終了コールバックの登録テーブル。
///
/// `play_with_callback` などで採番したトークン（`u32`、0 はコールバックなしの予約値）と
/// クロージャを対応付けて保持する。サウンドスレッドからは `Event::SourceFinished { token }`
/// 経由で完了通知が届くため、メインスレッド側で `complete()` を呼んでクロージャを取り出す。
pub(in crate::core) struct CallbackRegistry {
    callbacks: HashMap<u32, Box<dyn FnOnce() + Send>>,
    next_token: u32,
}

impl CallbackRegistry {
    pub(super) fn new() -> Self {
        Self {
            callbacks: HashMap::new(),
            next_token: 1,
        }
    }

    /// クロージャを登録し、対応するトークンを返す。
    pub(super) fn register(&mut self, callback: Box<dyn FnOnce() + Send>) -> u32 {
        let token = self.next_token;
        // wrapping_add(1).max(1) で 0 を飛ばし、コールバックなしと衝突させない。
        self.next_token = self.next_token.wrapping_add(1).max(1);
        self.callbacks.insert(token, callback);
        token
    }

    /// 登録を取り消す（コマンド push 失敗時のロールバック用）。
    pub(super) fn cancel(&mut self, token: u32) {
        self.callbacks.remove(&token);
    }

    /// 完了通知を受けてクロージャを取り出す。
    pub(super) fn complete(&mut self, token: u32) -> Option<Box<dyn FnOnce() + Send>> {
        self.callbacks.remove(&token)
    }

    /// 全コールバックを破棄する（`StopAll` 時のクリア用）。
    pub(super) fn clear(&mut self) {
        self.callbacks.clear();
    }
}
