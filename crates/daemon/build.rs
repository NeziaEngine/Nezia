//! `proto/nezia/v1/*.proto` から tonic / prost の Rust コードを OUT_DIR に生成する。
//!
//! 生成コードは git に commit しない (CONCEPT.md §7)。C# 側の生成は別経路。
//! `protoc` を PATH に要求する (CI / dev 環境に導入済み前提)。

use std::path::Path;

fn main() {
    // ワークスペースルートの proto/ を source of truth とする。
    // crates/daemon/ から見て 2 階層上。
    // canonicalize() は Windows で `\\?\` 付きの verbatim path を返し、protoc が
    // それを include path として解決できない ("File not found") ため使わない。
    // CARGO_MANIFEST_DIR は絶対パスなので parent() を辿るだけで正規化済みになる。
    let proto_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("ワークスペースルートが解決できない")
        .join("proto");
    assert!(
        proto_root.is_dir(),
        "proto/ ディレクトリが見つからない: {}",
        proto_root.display()
    );

    let daemon_proto = proto_root.join("nezia/v1/daemon.proto");
    let common_proto = proto_root.join("nezia/v1/common.proto");

    tonic_build::configure()
        .build_server(true)
        .build_client(false)
        .compile_protos(&[daemon_proto, common_proto], &[proto_root])
        .expect("proto コンパイルに失敗");
}
