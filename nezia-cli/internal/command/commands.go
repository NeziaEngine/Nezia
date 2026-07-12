package command

import (
	"flag"
	"fmt"
	"path/filepath"
	"strings"

	"jp.nezia/nezia-cli/internal/client"
	neziav1 "jp.nezia/nezia-cli/internal/gen/neziav1"
	"jp.nezia/nezia-cli/internal/output"
)

// parseAnywhere は位置引数の後に置かれたフラグも解釈する
// (標準 flag は最初の非フラグ引数でパースを打ち切るため)。位置引数を返す。
func parseAnywhere(fs *flag.FlagSet, args []string) ([]string, error) {
	var pos []string
	for {
		if err := fs.Parse(args); err != nil {
			return nil, err
		}
		args = fs.Args()
		if len(args) == 0 {
			return pos, nil
		}
		pos = append(pos, args[0])
		args = args[1:]
	}
}

func cmdPing(env *Env) int {
	c, err := env.dial()
	if err != nil {
		return env.fail(err)
	}
	ctx, cancel := env.Opts.Context()
	defer cancel()
	resp, err := c.PD.Ping(ctx, &neziav1.PingRequest{})
	if err != nil {
		return env.fail(client.MapRPCError(err))
	}
	// version handshake: 不一致は warning (stderr、契約出力は汚さない)。
	// どちらかが dev ビルドの間はスキーマ差の判定にならないため黙る。
	if version != "dev" && !strings.Contains(resp.GetVersion(), "dev") &&
		!strings.Contains(version, "dirty") && resp.GetVersion() != version {
		fmt.Fprintf(env.Stderr, "warning: version mismatch cli=%s daemon=%s\n", version, resp.GetVersion())
	}
	env.Format.OK(env.Stdout, output.KV{Key: "version", Value: resp.GetVersion()})
	return client.ExitOK
}

func cmdLoad(env *Env, args []string) int {
	if len(args) != 1 {
		return env.fail(&client.CodedError{Code: "INVALID_ARGUMENT", Msg: "usage: load <path>", Exit: client.ExitAppErr})
	}
	// daemon はカレントディレクトリが異なるため絶対パスで送る。
	path, err := filepath.Abs(args[0])
	if err != nil {
		return env.fail(&client.CodedError{Code: "INVALID_ARGUMENT", Msg: err.Error(), Exit: client.ExitAppErr})
	}
	c, err := env.dial()
	if err != nil {
		return env.fail(err)
	}
	ctx, cancel := env.Opts.Context()
	defer cancel()
	resp, err := c.PD.LoadBuffer(ctx, &neziav1.LoadBufferRequest{Path: path})
	if err != nil {
		return env.fail(client.MapRPCError(err))
	}
	b := resp.GetBuffer()
	h := client.Handle{Index: b.GetIndex(), Generation: b.GetGeneration()}
	env.Format.OK(env.Stdout, output.KV{Key: "buffer", Value: h.String()})
	return client.ExitOK
}

func cmdPlay(env *Env, args []string) int {
	fs := flag.NewFlagSet("play", flag.ContinueOnError)
	fs.SetOutput(env.Stderr)
	volume := fs.Float64("volume", 1.0, "linear gain")
	pitch := fs.Float64("pitch", 1.0, "playback rate")
	loop := fs.Bool("loop", false, "loop playback")
	bus := fs.String("bus", "", "target bus name (from mixer load)")
	pos, err := parseAnywhere(fs, args)
	if err != nil {
		return client.ExitAppErr
	}
	if len(pos) != 1 {
		return env.fail(&client.CodedError{Code: "INVALID_ARGUMENT", Msg: "usage: play <buffer> [--volume f] [--pitch f] [--loop] [--bus name]", Exit: client.ExitAppErr})
	}
	h, err := client.ParseHandle(pos[0])
	if err != nil {
		return env.fail(err)
	}
	c, err := env.dial()
	if err != nil {
		return env.fail(err)
	}
	ctx, cancel := env.Opts.Context()
	defer cancel()
	resp, err := c.PD.Play(ctx, &neziav1.PlayRequest{
		Buffer:  &neziav1.BufferId{Index: h.Index, Generation: h.Generation},
		Volume:  float32(*volume),
		Pitch:   float32(*pitch),
		Looping: *loop,
		Bus:     *bus,
	})
	if err != nil {
		return env.fail(client.MapRPCError(err))
	}
	s := resp.GetSource()
	sh := client.Handle{Index: s.GetIndex(), Generation: s.GetGeneration()}
	env.Format.OK(env.Stdout, output.KV{Key: "source", Value: sh.String()})
	return client.ExitOK
}

func cmdStop(env *Env, args []string) int {
	fs := flag.NewFlagSet("stop", flag.ContinueOnError)
	fs.SetOutput(env.Stderr)
	all := fs.Bool("all", false, "stop all sources")
	pos, err := parseAnywhere(fs, args)
	if err != nil {
		return client.ExitAppErr
	}

	c, err := env.dial()
	if err != nil {
		return env.fail(err)
	}
	ctx, cancel := env.Opts.Context()
	defer cancel()

	var accepted bool
	switch {
	case *all && len(pos) == 0:
		resp, err := c.PD.StopAll(ctx, &neziav1.StopAllRequest{})
		if err != nil {
			return env.fail(client.MapRPCError(err))
		}
		accepted = resp.GetAccepted()
	case !*all && len(pos) == 1:
		h, err := client.ParseHandle(pos[0])
		if err != nil {
			return env.fail(err)
		}
		resp, err := c.PD.Stop(ctx, &neziav1.StopRequest{
			Source: &neziav1.SourceHandle{Index: h.Index, Generation: h.Generation},
		})
		if err != nil {
			return env.fail(client.MapRPCError(err))
		}
		accepted = resp.GetAccepted()
	default:
		return env.fail(&client.CodedError{Code: "INVALID_ARGUMENT", Msg: "usage: stop <source> | stop --all", Exit: client.ExitAppErr})
	}

	// accepted=false はコマンドキュー満杯 or 既に無効なハンドル (daemon.proto)。
	if !accepted {
		return env.fail(&client.CodedError{Code: "REJECTED", Msg: "command not accepted (queue full or stale handle)", Exit: client.ExitAppErr})
	}
	env.Format.OK(env.Stdout)
	return client.ExitOK
}
