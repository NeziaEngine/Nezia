//go:build unix

package daemonctl

import (
	"os"
	"os/exec"
	"strconv"
	"strings"
	"syscall"
)

// verifyIsDaemon は pid が実際に nezia-daemon プロセスであることを確認する。
// port file の pid 再利用 (stale file の後に別プロセスが同じ pid を得た場合) で
// 無関係なプロセスへシグナルを送らないための安全チェック。
func verifyIsDaemon(pid int) (bool, error) {
	out, err := exec.Command("ps", "-p", strconv.Itoa(pid), "-o", "comm=").Output()
	if err != nil {
		// ps がプロセス無しで非ゼロ終了するのは「存在しない」を意味する。
		if _, ok := err.(*exec.ExitError); ok {
			return false, nil
		}
		return false, err
	}
	name := strings.TrimSpace(string(out))
	return strings.Contains(name, "nezia-daemon"), nil
}

func terminate(pid int) error {
	proc, err := os.FindProcess(pid)
	if err != nil {
		return err
	}
	return proc.Signal(syscall.SIGTERM)
}
