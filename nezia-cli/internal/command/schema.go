package command

import (
	"encoding/json"
	"fmt"

	"jp.nezia/nezia-cli/internal/client"
)

// schemaArg は 1 コマンド引数の機械可読な仕様。
type schemaArg struct {
	Name       string `json:"name"`
	Type       string `json:"type"`
	Positional bool   `json:"positional,omitempty"`
	Optional   bool   `json:"optional,omitempty"`
	Default    any    `json:"default,omitempty"`
}

type schemaCommand struct {
	Name   string      `json:"name"`
	Args   []schemaArg `json:"args"`
	Output any         `json:"output"`
}

type schemaDoc struct {
	Commands     []schemaCommand   `json:"commands"`
	ErrorCodes   []string          `json:"error_codes"`
	ExitCodes    map[string]string `json:"exit_codes"`
	HandleFormat string            `json:"handle_format"`
}

// cmdSchema は全コマンド・引数・出力形・エラーコードを 1 回で返す
// (docs/design/cli/CONCEPT.md §6)。エージェントは help を試行錯誤する代わりに
// これを 1 度読めば全操作を把握できる。出力形式は常にコンパクト JSON 固定
// (-o text は無視する。schema 自体が機械可読データであるため)。
func cmdSchema(env *Env) int {
	doc := schemaDoc{
		Commands: []schemaCommand{
			{Name: "ping", Output: map[string]string{"version": "string"}},
			{
				Name:   "load",
				Args:   []schemaArg{{Name: "path", Type: "string", Positional: true}},
				Output: map[string]string{"buffer": "string (<index>-<generation>)"},
			},
			{
				Name: "play",
				Args: []schemaArg{
					{Name: "buffer", Type: "string", Positional: true},
					{Name: "--volume", Type: "float", Default: 1.0},
					{Name: "--pitch", Type: "float", Default: 1.0},
					{Name: "--loop", Type: "bool", Default: false},
					{Name: "--bus", Type: "string", Optional: true},
					{Name: "--clip", Type: "string", Optional: true},
				},
				Output: map[string]string{"source": "string (<index>-<generation>)"},
			},
			{
				Name: "stop",
				Args: []schemaArg{
					{Name: "source", Type: "string", Positional: true, Optional: true},
					{Name: "--all", Type: "bool", Default: false},
				},
				Output: map[string]string{},
			},
			{Name: "daemon start", Output: map[string]string{"pid": "int", "port": "int"}},
			{
				Name:   "daemon stop",
				Args:   []schemaArg{{Name: "--pid", Type: "int", Optional: true}},
				Output: map[string]string{},
			},
			{
				Name: "daemon status",
				Args: []schemaArg{{Name: "--pid", Type: "int", Optional: true}},
				Output: map[string]string{
					"running": "bool", "pid": "int?", "port": "int?", "version": "string?",
				},
			},
			{
				Name:   "mixer load",
				Args:   []schemaArg{{Name: "file", Type: "string", Positional: true}},
				Output: map[string]string{"buses": "object (name -> bus handle)"},
			},
			{Name: "subscribe", Output: "JSONL stream: {event: source_stopped|play_failed|streaming_underrun|capture_overflow|subscriber_lagged, ...}"},
			{Name: "batch", Output: "stdin から 1 行 1 コマンド、stdout に 1 行 1 結果 (JSONL)"},
			{Name: "schema", Output: "this document"},
			{Name: "version", Output: map[string]string{"version": "string"}},
		},
		ErrorCodes: []string{
			"DAEMON_NOT_RUNNING", "DAEMON_AMBIGUOUS", "DAEMON_START_FAILED", "DAEMON_STOP_FAILED",
			"CONNECT_FAILED", "TIMEOUT", "INVALID_ARGUMENT", "INVALID_HANDLE", "REJECTED",
			"DAEMON_ERROR", "UNKNOWN_COMMAND", "INTERNAL",
		},
		ExitCodes: map[string]string{
			"0": "success", "1": "application error", "2": "connection error",
		},
		HandleFormat: "<index>-<generation> (例: 3-1)",
	}
	b, err := json.Marshal(doc)
	if err != nil {
		return env.fail(&client.CodedError{Code: "INTERNAL", Msg: err.Error(), Exit: client.ExitAppErr})
	}
	fmt.Fprintln(env.Stdout, string(b))
	return client.ExitOK
}
