// Package daemonctl は GUI を持たないヘッドレスセッション向けに nezia-daemon の
// ライフサイクルを明示管理する (docs/design/cli/CONCEPT.md §8)。
//
// daemon の既存スタンドアロンモード (--parent-pid なし) を利用する。このモードでは
// port discovery file のキーが daemon 自身の PID になり (lifecycle.rs)、self-exit
// もしないため、cli 側が明示的に stop するまで生き続ける。
//
// 重要な安全条件: Stop/Status が対象を選ぶのは port file の glob だけであり、
// Editor 等が --parent-pid 付きで spawn した daemon (キー = 親の PID) も同じ
// glob にヒットしうる。誤って無関係なプロセス (例えば Editor 本体) へシグナルを
// 送らないよう、シグナル送信前に必ず `verifyIsDaemon` でプロセス名を確認する。
package daemonctl

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

type NotFoundError struct{ Detail string }

func (e *NotFoundError) Error() string { return e.Detail }

type AmbiguousError struct{ Candidates []int }

func (e *AmbiguousError) Error() string {
	return fmt.Sprintf("multiple daemons running: pids %v (pass --pid)", e.Candidates)
}

// Session は 1 個の稼働中 daemon (port file から見つかったもの)。
type Session struct {
	PID  int
	Port int
}

func portFilePath(pid int) string {
	return filepath.Join(os.TempDir(), fmt.Sprintf("nezia-daemon-%d.port", pid))
}

// Discover は port file を PID で解決する。pid が 0 なら glob して単一候補を探す。
func Discover(pid int) (Session, error) {
	if pid != 0 {
		port, err := readPort(portFilePath(pid))
		if err != nil {
			return Session{}, &NotFoundError{Detail: fmt.Sprintf("no daemon with pid %d: %v", pid, err)}
		}
		return Session{PID: pid, Port: port}, nil
	}

	matches, _ := filepath.Glob(filepath.Join(os.TempDir(), "nezia-daemon-*.port"))
	if len(matches) == 0 {
		return Session{}, &NotFoundError{Detail: "no daemon port file found"}
	}
	var candidates []int
	for _, m := range matches {
		if p := extractPID(m); p != 0 {
			candidates = append(candidates, p)
		}
	}
	if len(candidates) == 0 {
		return Session{}, &NotFoundError{Detail: "no daemon port file found"}
	}
	if len(candidates) > 1 {
		return Session{}, &AmbiguousError{Candidates: candidates}
	}
	port, err := readPort(portFilePath(candidates[0]))
	if err != nil {
		return Session{}, &NotFoundError{Detail: err.Error()}
	}
	return Session{PID: candidates[0], Port: port}, nil
}

func extractPID(portFile string) int {
	base := filepath.Base(portFile)
	base = strings.TrimPrefix(base, "nezia-daemon-")
	base = strings.TrimSuffix(base, ".port")
	pid, err := strconv.Atoi(base)
	if err != nil {
		return 0
	}
	return pid
}

func readPort(path string) (int, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return 0, err
	}
	port, err := strconv.Atoi(strings.TrimSpace(string(b)))
	if err != nil || port <= 0 || port > 65535 {
		return 0, fmt.Errorf("invalid port file content %q", strings.TrimSpace(string(b)))
	}
	return port, nil
}

// BinaryPath は起動する nezia-daemon の実行ファイルを解決する。
// NEZIA_DAEMON_BIN → PATH 上の "nezia-daemon" の順に探す。
func BinaryPath() (string, error) {
	if env := os.Getenv("NEZIA_DAEMON_BIN"); env != "" {
		return env, nil
	}
	path, err := exec.LookPath("nezia-daemon")
	if err != nil {
		return "", fmt.Errorf("nezia-daemon not found (set NEZIA_DAEMON_BIN or add it to PATH): %w", err)
	}
	return path, nil
}

// Start は nezia-daemon をスタンドアロンモード (--parent-pid なし) で spawn し、
// port file の出現を待ってから Session を返す。
func Start(timeout time.Duration) (Session, error) {
	bin, err := BinaryPath()
	if err != nil {
		return Session{}, err
	}
	cmd := exec.Command(bin)
	// daemon の stdout/stderr は破棄する (cli の契約出力を汚さないため)。
	cmd.Stdout = nil
	cmd.Stderr = nil
	if err := cmd.Start(); err != nil {
		return Session{}, fmt.Errorf("spawn %s: %w", bin, err)
	}
	pid := cmd.Process.Pid
	// 親プロセス (cli) の終了後もセッションとして生き続けさせるため、
	// 子プロセスの終了を待たずに切り離す (Wait しない = zombie 回収は OS 任せ)。
	// stop 時に明示的に Wait 相当の後始末を行うのは行わない (daemon 自身が
	// port file を消して抜けるので、cli 側で pid の再利用検知は不要)。

	deadline := time.Now().Add(timeout)
	portPath := portFilePath(pid)
	for time.Now().Before(deadline) {
		if port, err := readPort(portPath); err == nil {
			return Session{PID: pid, Port: port}, nil
		}
		time.Sleep(50 * time.Millisecond)
	}
	return Session{}, fmt.Errorf("daemon (pid %d) did not write port file within %s", pid, timeout)
}

// Stop は対象 daemon を graceful shutdown する。SIGTERM (windows は Kill) を送り、
// port file が消えるまで待つ。
func Stop(sess Session, timeout time.Duration) error {
	ok, err := verifyIsDaemon(sess.PID)
	if err != nil {
		return fmt.Errorf("could not verify pid %d: %w", sess.PID, err)
	}
	if !ok {
		// stale な port file (daemon は既に落ちている)。実害はないので片付けるだけ。
		_ = os.Remove(portFilePath(sess.PID))
		return nil
	}
	if err := terminate(sess.PID); err != nil {
		return fmt.Errorf("terminate pid %d: %w", sess.PID, err)
	}

	deadline := time.Now().Add(timeout)
	portPath := portFilePath(sess.PID)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(portPath); os.IsNotExist(err) {
			return nil
		}
		time.Sleep(50 * time.Millisecond)
	}
	return fmt.Errorf("daemon (pid %d) did not exit within %s", sess.PID, timeout)
}
