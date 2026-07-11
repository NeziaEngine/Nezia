package command

import (
	"bufio"
	"strings"

	"jp.nezia/nezia-cli/internal/client"
)

// cmdBatch は stdin から 1 行 1 コマンドを読み、stdout に 1 行 1 結果を返す常駐モード。
// gRPC 接続 (env.dial) を全行で使い回すことで、反復操作のプロセス起動・接続コストを
// 1 回に抑える (docs/design/cli/CONCEPT.md §6)。
//
// 制限: 各行は strings.Fields で単純に空白分割する。引数にスペースを含むパス
// (例: load "My Song.wav") は非対応。
func cmdBatch(env *Env, in *bufio.Scanner) int {
	lastExit := client.ExitOK
	for in.Scan() {
		line := strings.TrimSpace(in.Text())
		if line == "" {
			continue
		}
		fields := strings.Fields(line)
		lastExit = Dispatch(env, fields[0], fields[1:])
	}
	return lastExit
}
