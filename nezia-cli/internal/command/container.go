package command

import (
	"flag"

	"jp.nezia/nezia-cli/internal/client"
	neziav1 "jp.nezia/nezia-cli/internal/gen/neziav1"
	"jp.nezia/nezia-cli/internal/output"
)

// cmdContainer は container サブコマンド群 (create / play / destroy)。
func cmdContainer(env *Env, args []string) int {
	if len(args) == 0 {
		return env.fail(&client.CodedError{
			Code: "INVALID_ARGUMENT",
			Msg:  "usage: container create <buffer...> | container play <container> | container destroy <container>",
			Exit: client.ExitAppErr,
		})
	}
	switch args[0] {
	case "create":
		return cmdContainerCreate(env, args[1:])
	case "play":
		return cmdContainerPlay(env, args[1:])
	case "destroy":
		return cmdContainerDestroy(env, args[1:])
	default:
		return env.fail(&client.CodedError{
			Code: "INVALID_ARGUMENT",
			Msg:  "usage: container create|play|destroy",
			Exit: client.ExitAppErr,
		})
	}
}

func cmdContainerCreate(env *Env, args []string) int {
	if len(args) == 0 {
		return env.fail(&client.CodedError{
			Code: "INVALID_ARGUMENT",
			Msg:  "usage: container create <buffer> [<buffer>...]",
			Exit: client.ExitAppErr,
		})
	}
	var children []*neziav1.BufferId
	for _, arg := range args {
		h, err := client.ParseHandle(arg)
		if err != nil {
			return env.fail(err)
		}
		children = append(children, &neziav1.BufferId{Index: h.Index, Generation: h.Generation})
	}
	c, err := env.dial()
	if err != nil {
		return env.fail(err)
	}
	defer c.Close()
	ctx, cancel := env.Opts.Context()
	defer cancel()
	resp, err := c.PD.CreateContainer(ctx, &neziav1.CreateContainerRequest{Children: children})
	if err != nil {
		return env.fail(client.MapRPCError(err))
	}
	ch := resp.GetContainer()
	h := client.Handle{Index: ch.GetIndex(), Generation: ch.GetGeneration()}
	env.Format.OK(env.Stdout, output.KV{Key: "container", Value: h.String()})
	return client.ExitOK
}

func cmdContainerPlay(env *Env, args []string) int {
	fs := flag.NewFlagSet("container play", flag.ContinueOnError)
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
		return env.fail(&client.CodedError{
			Code: "INVALID_ARGUMENT",
			Msg:  "usage: container play <container> [--volume f] [--pitch f] [--loop] [--bus name]",
			Exit: client.ExitAppErr,
		})
	}
	h, err := client.ParseHandle(pos[0])
	if err != nil {
		return env.fail(err)
	}
	c, err := env.dial()
	if err != nil {
		return env.fail(err)
	}
	defer c.Close()
	ctx, cancel := env.Opts.Context()
	defer cancel()
	resp, err := c.PD.PlayContainer(ctx, &neziav1.PlayContainerRequest{
		Container: &neziav1.ContainerHandle{Index: h.Index, Generation: h.Generation},
		Volume:    float32(*volume),
		Pitch:     float32(*pitch),
		Looping:   *loop,
		Bus:       *bus,
	})
	if err != nil {
		return env.fail(client.MapRPCError(err))
	}
	s := resp.GetSource()
	sh := client.Handle{Index: s.GetIndex(), Generation: s.GetGeneration()}
	env.Format.OK(env.Stdout, output.KV{Key: "source", Value: sh.String()})
	return client.ExitOK
}

func cmdContainerDestroy(env *Env, args []string) int {
	if len(args) != 1 {
		return env.fail(&client.CodedError{
			Code: "INVALID_ARGUMENT",
			Msg:  "usage: container destroy <container>",
			Exit: client.ExitAppErr,
		})
	}
	h, err := client.ParseHandle(args[0])
	if err != nil {
		return env.fail(err)
	}
	c, err := env.dial()
	if err != nil {
		return env.fail(err)
	}
	defer c.Close()
	ctx, cancel := env.Opts.Context()
	defer cancel()
	resp, err := c.PD.DestroyContainer(ctx, &neziav1.DestroyContainerRequest{
		Container: &neziav1.ContainerHandle{Index: h.Index, Generation: h.Generation},
	})
	if err != nil {
		return env.fail(client.MapRPCError(err))
	}
	if !resp.GetDestroyed() {
		return env.fail(&client.CodedError{
			Code: "INVALID_HANDLE",
			Msg:  "container not found or stale handle",
			Exit: client.ExitAppErr,
		})
	}
	env.Format.OK(env.Stdout)
	return client.ExitOK
}
