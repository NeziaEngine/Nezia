//! プロセスライフサイクル: port discovery file と parent PID 監視。
//!
//! daemon CONCEPT.md §2 / §5:
//! - OS が割り当てた ephemeral port を `$TMPDIR/nezia-daemon-{parent_pid}.port` に書き、
//!   Editor はそのファイルを読んで接続する (LSP / DAP と同じ標準パターン)。
//! - `--parent-pid` を 1Hz で監視し、親 (Editor) が消えたら self-exit する。
//!   これにより Editor がクラッシュしても孤児プロセスが残らない。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// port discovery file のパスを組み立てる。
///
/// `parent_pid` が `Some` ならそれを、`None` (スタンドアロン起動) なら自身の pid を
/// ファイル名のキーにする。
pub fn port_file_path(parent_pid: Option<u32>) -> PathBuf {
    let key = parent_pid.unwrap_or_else(std::process::id);
    std::env::temp_dir().join(format!("nezia-daemon-{key}.port"))
}

/// 割り当てられた port を discovery file に書き出す。
pub fn write_port_file(path: &Path, port: u16) -> io::Result<()> {
    fs::write(path, port.to_string())
}

/// 親プロセスを 1Hz で監視し、消失したら port file を片付けて self-exit する。
///
/// tokio タスクとして spawn する想定。戻り値は無い (exit するか daemon と運命を共にする)。
pub async fn monitor_parent(parent_pid: u32, port_file: PathBuf) {
    let interval = Duration::from_secs(1);
    loop {
        tokio::time::sleep(interval).await;
        if !parent_alive(parent_pid) {
            // 先に後始末を済ませ、ログは最後 + 失敗無視で書く。親が stdout/stderr を
            // パイプで奪ったまま死ぬと書き込みが EPIPE になり、`eprintln!` は panic
            // する。panic するとこの監視タスクだけが落ちて daemon が孤児として
            // 生き残る (Unity Editor の Process 起動 + リダイレクトで実際に発生)。
            let _ = fs::remove_file(&port_file);
            {
                use std::io::Write;
                let _ = writeln!(
                    std::io::stderr(),
                    "nezia-daemon: parent process {parent_pid} is gone, exiting"
                );
            }
            std::process::exit(0);
        }
    }
}

/// 親プロセスが生存しているか。
#[cfg(unix)]
fn parent_alive(pid: u32) -> bool {
    // kill(pid, 0) はシグナルを送らず存在チェックのみ行う。
    //   0      → プロセスは存在する
    //   EPERM  → 存在するが権限が無い (= 生存とみなす)
    //   ESRCH  → 存在しない
    // SAFETY: kill は任意の pid / sig=0 で安全に呼べる純粋な存在問い合わせ。
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if ret == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// 親プロセスが生存しているか (Windows)。
#[cfg(windows)]
fn parent_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    // SAFETY: OpenProcess は任意の pid に対して安全に呼べる。失敗時は NULL を返す。
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        // プロセスが存在しない (or 既に終了) → 親は消えたとみなす。
        return false;
    }
    // 0ms タイムアウトで現在のシグナル状態のみ問い合わせる。
    // WAIT_TIMEOUT = まだ生存 / WAIT_OBJECT_0 = 終了済み。
    // SAFETY: handle は OpenProcess が返した有効なハンドル。
    let wait = unsafe { WaitForSingleObject(handle, 0) };
    // SAFETY: 同上。ハンドルは以後使わないのでここで閉じる。
    unsafe { CloseHandle(handle) };
    wait == WAIT_TIMEOUT
}
