//go:build windows

package daemonctl

import (
	"os"
	"os/exec"
	"strconv"
	"strings"
)

// verifyIsDaemon は pid が実際に nezia-daemon.exe プロセスであることを確認する。
func verifyIsDaemon(pid int) (bool, error) {
	out, err := exec.Command("tasklist", "/FI", "PID eq "+strconv.Itoa(pid), "/FO", "CSV", "/NH").Output()
	if err != nil {
		return false, err
	}
	return strings.Contains(strings.ToLower(string(out)), "nezia-daemon"), nil
}

// Windows には SIGTERM 相当の graceful シグナルが無いため強制終了する。
// daemon 側のクリーンアップ (port file 削除) は走らない可能性があるため、
// stop 後に daemonctl 側で port file を明示的に削除する。
func terminate(pid int) error {
	proc, err := os.FindProcess(pid)
	if err != nil {
		return err
	}
	if err := proc.Kill(); err != nil {
		return err
	}
	_ = os.Remove(portFilePath(pid))
	return nil
}
