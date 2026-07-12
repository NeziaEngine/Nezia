package command

import (
	"fmt"
	"os"
	"strings"

	"google.golang.org/protobuf/encoding/protojson"

	"jp.nezia/nezia-cli/internal/client"
	neziav1 "jp.nezia/nezia-cli/internal/gen/neziav1"
	"jp.nezia/nezia-cli/internal/output"
)

// cmdMixer は mixer サブコマンド群のディスパッチ。現状は load のみ。
func cmdMixer(env *Env, args []string) int {
	if len(args) == 0 || args[0] != "load" {
		return env.fail(&client.CodedError{
			Code: "INVALID_ARGUMENT",
			Msg:  "usage: mixer load <file.json>",
			Exit: client.ExitAppErr,
		})
	}
	return cmdMixerLoad(env, args[1:])
}

// cmdMixerLoad は JSON ファイルを MixerDef (proto) に読み込み daemon へ送る。
//
// JSON スキーマは proto の MixerDef そのもの (protojson)。フィールド名は
// lowerCamelCase / snake_case のどちらでも受け付ける。例:
//
//	{"buses":[{"name":"BGM","gain":1.0,
//	           "effects":[{"position":"CHAIN_POSITION_PRE","enabled":true,
//	                       "lowPass":{"cutoff":800,"q":0.7}}]}],
//	 "sends":[{"sourceBus":"BGM","targetBus":"ReverbBus","gain":0.5}]}
func cmdMixerLoad(env *Env, args []string) int {
	if len(args) != 1 {
		return env.fail(&client.CodedError{
			Code: "INVALID_ARGUMENT",
			Msg:  "usage: mixer load <file.json>",
			Exit: client.ExitAppErr,
		})
	}
	data, err := os.ReadFile(args[0])
	if err != nil {
		return env.fail(&client.CodedError{
			Code: "INVALID_ARGUMENT",
			Msg:  fmt.Sprintf("read %s: %v", args[0], err),
			Exit: client.ExitAppErr,
		})
	}
	def := &neziav1.MixerDef{}
	if err := protojson.Unmarshal(data, def); err != nil {
		return env.fail(&client.CodedError{
			Code: "INVALID_ARGUMENT",
			Msg:  fmt.Sprintf("parse %s: %v", args[0], err),
			Exit: client.ExitAppErr,
		})
	}

	c, err := env.dial()
	if err != nil {
		return env.fail(err)
	}
	defer c.Close()
	ctx, cancel := env.Opts.Context()
	defer cancel()
	resp, err := c.PD.LoadMixer(ctx, &neziav1.LoadMixerRequest{Mixer: def})
	if err != nil {
		return env.fail(client.MapRPCError(err))
	}

	// name=handle を生成順で並べる (JSON では 1 オブジェクト、text では空白区切り)。
	switch env.Format {
	case output.Text:
		var b strings.Builder
		b.WriteString("ok")
		for _, nb := range resp.GetBuses() {
			h := client.Handle{Index: nb.GetBus().GetIndex(), Generation: nb.GetBus().GetGeneration()}
			fmt.Fprintf(&b, " %s=%s", nb.GetName(), h)
		}
		fmt.Fprintln(env.Stdout, b.String())
	default:
		var b strings.Builder
		b.WriteString(`{"ok":true,"buses":{`)
		for i, nb := range resp.GetBuses() {
			if i > 0 {
				b.WriteString(",")
			}
			h := client.Handle{Index: nb.GetBus().GetIndex(), Generation: nb.GetBus().GetGeneration()}
			fmt.Fprintf(&b, `%q:%q`, nb.GetName(), h.String())
		}
		b.WriteString("}}")
		fmt.Fprintln(env.Stdout, b.String())
	}
	return client.ExitOK
}
