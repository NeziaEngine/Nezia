//! NEZIA ENGINE preview daemon (骨格)。
//!
//! 実機ランタイムと同一の `nezia-core` を別プロセスで駆動し、Editor /
//! オーサリングツールからの gRPC でアセットを試聴する out-of-process ホスト。
//! 設計は docs/design/daemon/CONCEPT.md を正とする。
//!
//! 0.2.0 Tier 1 スコープ: LoadBuffer / Play / Stop / StopAll / Ping。
//!
//! ## 起動
//! ```text
//! nezia-daemon --parent-pid <Editor の PID>
//! ```
//! 起動すると 127.0.0.1 の ephemeral port に bind し、その番号を
//! `$TMPDIR/nezia-daemon-{parent_pid}.port` に書き出す。Editor は同ファイルを
//! 読んで接続する。親 PID が消えたら self-exit する。

mod engine;
mod lifecycle;
mod proto;
mod service;

use std::error::Error;

use tonic::transport::Server;

use crate::proto::v1::preview_daemon_server::PreviewDaemonServer;
use crate::service::PreviewService;

/// コマンドライン引数。
struct Args {
    /// 監視対象の親プロセス PID。省略時は監視しない (スタンドアロン動作)。
    parent_pid: Option<u32>,
}

fn parse_args() -> Result<Args, String> {
    let mut parent_pid = None;
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix("--parent-pid=") {
            parent_pid = Some(parse_pid(value)?);
        } else if arg == "--parent-pid" {
            let value = iter
                .next()
                .ok_or("--parent-pid requires a value".to_string())?;
            parent_pid = Some(parse_pid(&value)?);
        } else {
            return Err(format!("unknown argument: {arg}"));
        }
    }
    Ok(Args { parent_pid })
}

fn parse_pid(value: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .map_err(|_| format!("invalid --parent-pid value: {value}"))
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;

    // SoundEngine (cpal デバイス確保) を専用スレッドで起動する。!Send なため
    // tokio runtime より前に、メインスレッドとは別の場所で初期化する。
    let engine = engine::spawn()?;

    // tonic server 用のマルチスレッド runtime。
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve(engine, args.parent_pid))
}

async fn serve(
    engine: engine::EngineHandle,
    parent_pid: Option<u32>,
) -> Result<(), Box<dyn Error>> {
    // 127.0.0.1:0 へ bind し、OS が割り当てた ephemeral port を取得する。
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let addr = listener.local_addr()?;

    // port discovery file を書き出す。Editor はこれを読んで接続する。
    let port_file = lifecycle::port_file_path(parent_pid);
    lifecycle::write_port_file(&port_file, addr.port())?;
    eprintln!(
        "nezia-daemon: listening on {addr} (port file: {})",
        port_file.display()
    );

    // 親プロセス監視 (CONCEPT.md §5)。
    if let Some(ppid) = parent_pid {
        tokio::spawn(lifecycle::monitor_parent(ppid, port_file.clone()));
    }

    let service = PreviewService::new(engine, env!("CARGO_PKG_VERSION"));
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    // Ctrl-C (SIGINT) または SIGTERM で graceful shutdown する。
    // SIGTERM を無視すると port discovery file の後始末 (下記) が走らず、
    // 次回起動時に stale なファイルが残って接続解決を誤らせる。
    let shutdown = shutdown_signal();

    let result = Server::builder()
        .add_service(PreviewDaemonServer::new(service))
        .serve_with_incoming_shutdown(incoming, shutdown)
        .await;

    // discovery file を後始末する (best effort)。
    let _ = std::fs::remove_file(&port_file);

    result.map_err(Into::into)
}

/// SIGINT (Ctrl-C) または SIGTERM を待つ。Windows には SIGTERM 相当が
/// ないため ctrl_c のみを待つ。
#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm =
        signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => eprintln!("nezia-daemon: SIGINT received"),
        _ = sigterm.recv() => eprintln!("nezia-daemon: SIGTERM received"),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    eprintln!("nezia-daemon: shutdown signal received");
}
