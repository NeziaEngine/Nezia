// Package output は stdout への整形出力を担う。
//
// 契約 (docs/design/cli/CONCEPT.md §4):
//   - 1 メッセージ = 1 行。診断・ログは stderr へ。
//   - 成功形 {"ok":true,...} / 失敗形 {"ok":false,"error":{"code","msg"}} に統一。
//   - -o text はさらにトークンを削った行指向形式。
package output

import (
	"encoding/json"
	"fmt"
	"io"
	"strings"
)

type Format int

const (
	JSON Format = iota
	Text
)

// ParseFormat は -o フラグの値を解釈する。
func ParseFormat(s string) (Format, error) {
	switch s {
	case "json", "":
		return JSON, nil
	case "text":
		return Text, nil
	default:
		return JSON, fmt.Errorf("unknown output format %q (json|text)", s)
	}
}

// KV は出力フィールド。キー順を安定させるため map ではなくスライスで持つ。
type KV struct {
	Key   string
	Value any
}

// OK は成功メッセージを 1 行出力する。
func (f Format) OK(w io.Writer, pairs ...KV) {
	switch f {
	case Text:
		var b strings.Builder
		b.WriteString("ok")
		for _, kv := range pairs {
			fmt.Fprintf(&b, " %s=%v", kv.Key, kv.Value)
		}
		fmt.Fprintln(w, b.String())
	default:
		var b strings.Builder
		b.WriteString(`{"ok":true`)
		for _, kv := range pairs {
			v, err := json.Marshal(kv.Value)
			if err != nil {
				v = []byte(`null`)
			}
			fmt.Fprintf(&b, `,%q:%s`, kv.Key, v)
		}
		b.WriteString("}")
		fmt.Fprintln(w, b.String())
	}
}

// Err は失敗メッセージを 1 行出力する。
func (f Format) Err(w io.Writer, code, msg string) {
	switch f {
	case Text:
		fmt.Fprintf(w, "err %s: %s\n", code, msg)
	default:
		m, _ := json.Marshal(msg)
		fmt.Fprintf(w, `{"ok":false,"error":{"code":%q,"msg":%s}}`+"\n", code, m)
	}
}
