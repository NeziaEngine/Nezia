// Package command はサブコマンドのパースと実行を担う。
//
// 契約: サブコマンド + stdout の 1 行 JSON (docs/design/cli/CONCEPT.md §3–5)。
// help はトップレベル 40 行以内を維持する (同 §6、CI でチェック)。
package command

import (
	"bufio"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"time"

	"jp.nezia/nezia-cli/internal/client"
	"jp.nezia/nezia-cli/internal/output"
)

// version は Makefile の -ldflags で埋め込む。
var version = "dev"

const usage = `nezia-cli — NEZIA ENGINE daemon front door

Usage:
  nezia-cli [global flags] <command> [args]

Commands:
  ping                       daemon の疎通確認とバージョン取得
  load <path>                オーディオファイルをロードし buffer handle を返す
  play <buffer> [flags]      buffer を再生し source handle を返す
      --volume <f> --pitch <f> --loop
      --bus <name>           出力先バス (mixer load の論理名、省略 = Master)
  stop <source>              source を停止する
  stop --all                 全 source を停止する
  daemon start                ヘッドレス daemon を起動 (pid/port を返す)
  daemon stop [--pid n]      daemon を停止 (省略時は自動検出)
  daemon status [--pid n]    daemon の稼働状況を取得
  mixer load <file.json>     ミキサー構成 (バス/エフェクト/send) を一括ロード
  subscribe                  エンジンイベントを JSONL でストリーム受信
  batch                      stdin から 1 行 1 コマンドを読み常駐実行
  schema                     全コマンド仕様を機械可読 JSON で出力
  version                    cli 自身のバージョン

Global flags:
  -o <json|text>             出力形式 (default json)
  --port <n>                 daemon の port を直接指定
  --parent-pid <pid>         port discovery のキー (env: NEZIA_DAEMON_PID)
  --timeout <dur>            RPC タイムアウト (default 10s)

Output: 1 行 JSON {"ok":true,...} / {"ok":false,"error":{"code","msg"}}
Exit:   0=成功 1=エラー 2=daemon 未接続
Handle: <index>-<generation> 形式 (例: 3-1)
`

// Env は実行環境。テストで stdout/stderr を差し替える。
type Env struct {
	Stdout io.Writer
	Stderr io.Writer
	Stdin  io.Reader
	Format output.Format
	Opts   client.Options
	// DialFunc はテストでフェイク接続に差し替える。nil なら client.Dial。
	DialFunc func(client.Options) (*client.Client, error)

	conn *client.Client // dial() がメモ化する。batch モードで複数コマンド間で使い回す。
}

// dial は接続をメモ化する。プロセス起動 1 回・接続 1 回にとどめたい batch モード
// (docs/design/cli/CONCEPT.md §6) のために、Env の寿命内では同じ接続を再利用する。
func (e *Env) dial() (*client.Client, error) {
	if e.conn != nil {
		return e.conn, nil
	}
	var (
		c   *client.Client
		err error
	)
	if e.DialFunc != nil {
		c, err = e.DialFunc(e.Opts)
	} else {
		c, err = client.Dial(e.Opts)
	}
	if err != nil {
		return nil, err
	}
	e.conn = c
	return c, nil
}

// Close は保持中の接続を閉じる。batch モードの終了時に呼ぶ。
func (e *Env) Close() {
	if e.conn != nil {
		e.conn.Close()
		e.conn = nil
	}
}

// fail はエラーを契約どおり stdout に 1 行出力し、exit code を返す。
func (e *Env) fail(err error) int {
	var ce *client.CodedError
	if errors.As(err, &ce) {
		e.Format.Err(e.Stdout, ce.Code, ce.Msg)
		return ce.Exit
	}
	e.Format.Err(e.Stdout, "INTERNAL", err.Error())
	return client.ExitAppErr
}

// Run が cli のエントリポイント。exit code を返す。
func Run(args []string, stdin io.Reader, stdout, stderr io.Writer) int {
	global := flag.NewFlagSet("nezia-cli", flag.ContinueOnError)
	global.SetOutput(stderr)
	global.Usage = func() { fmt.Fprint(stderr, usage) }
	formatFlag := global.String("o", "json", "")
	port := global.Int("port", 0, "")
	parentPID := global.Int("parent-pid", 0, "")
	timeout := global.Duration("timeout", 10*time.Second, "")
	if err := global.Parse(args); err != nil {
		return client.ExitAppErr
	}

	format, err := output.ParseFormat(*formatFlag)
	env := &Env{
		Stdout: stdout,
		Stderr: stderr,
		Stdin:  stdin,
		Format: format,
		Opts:   client.Options{Port: *port, ParentPID: *parentPID, Timeout: *timeout},
	}
	if err != nil {
		return env.fail(&client.CodedError{Code: "INVALID_ARGUMENT", Msg: err.Error(), Exit: client.ExitAppErr})
	}

	rest := global.Args()
	if len(rest) == 0 {
		global.Usage()
		return client.ExitAppErr
	}
	defer env.Close()
	return Dispatch(env, rest[0], rest[1:])
}

// Dispatch はサブコマンド名で分岐する。
func Dispatch(env *Env, name string, args []string) int {
	switch name {
	case "ping":
		return cmdPing(env)
	case "load":
		return cmdLoad(env, args)
	case "play":
		return cmdPlay(env, args)
	case "stop":
		return cmdStop(env, args)
	case "daemon":
		return cmdDaemon(env, args)
	case "batch":
		stdin := env.Stdin
		if stdin == nil {
			stdin = os.Stdin
		}
		return cmdBatch(env, bufio.NewScanner(stdin))
	case "mixer":
		return cmdMixer(env, args)
	case "subscribe":
		return cmdSubscribe(env)
	case "schema":
		return cmdSchema(env)
	case "version":
		env.Format.OK(env.Stdout, output.KV{Key: "version", Value: version})
		return client.ExitOK
	case "help", "-h", "--help":
		fmt.Fprint(env.Stderr, usage)
		return client.ExitOK
	default:
		return env.fail(&client.CodedError{
			Code: "UNKNOWN_COMMAND",
			Msg:  fmt.Sprintf("%q is not a command (see nezia-cli help)", name),
			Exit: client.ExitAppErr,
		})
	}
}

// Main は os 直結のエントリポイント。
func Main() {
	os.Exit(Run(os.Args[1:], os.Stdin, os.Stdout, os.Stderr))
}
