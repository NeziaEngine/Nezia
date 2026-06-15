//! tonic-build が OUT_DIR に生成した gRPC コードを取り込む。
//!
//! 生成元は `proto/nezia/v1/*.proto` (build.rs 参照)。パッケージ `nezia.v1` の
//! メッセージ / service が `crate::proto::v1` 配下に展開される。

pub mod v1 {
    tonic::include_proto!("nezia.v1");
}
