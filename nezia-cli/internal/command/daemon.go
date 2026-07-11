package command

import (
	"context"
	"flag"
	"time"

	"jp.nezia/nezia-cli/internal/client"
	"jp.nezia/nezia-cli/internal/daemonctl"
	neziav1 "jp.nezia/nezia-cli/internal/gen/neziav1"
	"jp.nezia/nezia-cli/internal/output"
)

// startFunc/stopFunc/discoverFunc はテストでフェイクに差し替える。
var (
	startFunc    = daemonctl.Start
	stopFunc     = daemonctl.Stop
	discoverFunc = daemonctl.Discover
)

func cmdDaemon(env *Env, args []string) int {
	if len(args) == 0 {
		return env.fail(&client.CodedError{Code: "INVALID_ARGUMENT", Msg: "usage: daemon start|stop|status", Exit: client.ExitAppErr})
	}
	switch args[0] {
	case "start":
		return cmdDaemonStart(env, args[1:])
	case "stop":
		return cmdDaemonStop(env, args[1:])
	case "status":
		return cmdDaemonStatus(env, args[1:])
	default:
		return env.fail(&client.CodedError{Code: "INVALID_ARGUMENT", Msg: "usage: daemon start|stop|status", Exit: client.ExitAppErr})
	}
}

func cmdDaemonStart(env *Env, args []string) int {
	fs := flag.NewFlagSet("daemon start", flag.ContinueOnError)
	fs.SetOutput(env.Stderr)
	if _, err := parseAnywhere(fs, args); err != nil {
		return client.ExitAppErr
	}

	sess, err := startFunc(startupTimeout(env))
	if err != nil {
		return env.fail(&client.CodedError{Code: "DAEMON_START_FAILED", Msg: err.Error(), Exit: client.ExitAppErr})
	}
	env.Format.OK(env.Stdout, output.KV{Key: "pid", Value: sess.PID}, output.KV{Key: "port", Value: sess.Port})
	return client.ExitOK
}

func cmdDaemonStop(env *Env, args []string) int {
	fs := flag.NewFlagSet("daemon stop", flag.ContinueOnError)
	fs.SetOutput(env.Stderr)
	pid := fs.Int("pid", 0, "target daemon pid (default: auto-detect)")
	if _, err := parseAnywhere(fs, args); err != nil {
		return client.ExitAppErr
	}

	sess, err := discoverFunc(*pid)
	if err != nil {
		return env.fail(discoverErrToCoded(err))
	}
	if err := stopFunc(sess, startupTimeout(env)); err != nil {
		return env.fail(&client.CodedError{Code: "DAEMON_STOP_FAILED", Msg: err.Error(), Exit: client.ExitAppErr})
	}
	env.Format.OK(env.Stdout, output.KV{Key: "pid", Value: sess.PID})
	return client.ExitOK
}

func cmdDaemonStatus(env *Env, args []string) int {
	fs := flag.NewFlagSet("daemon status", flag.ContinueOnError)
	fs.SetOutput(env.Stderr)
	pid := fs.Int("pid", 0, "target daemon pid (default: auto-detect)")
	if _, err := parseAnywhere(fs, args); err != nil {
		return client.ExitAppErr
	}

	sess, err := discoverFunc(*pid)
	if err != nil {
		var nf *daemonctl.NotFoundError
		if isNotFound(err, &nf) {
			env.Format.OK(env.Stdout, output.KV{Key: "running", Value: false})
			return client.ExitOK
		}
		return env.fail(discoverErrToCoded(err))
	}

	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	conn, dialErr := client.DialAddr(sess.Port)
	if dialErr != nil {
		env.Format.OK(env.Stdout, output.KV{Key: "running", Value: false}, output.KV{Key: "pid", Value: sess.PID})
		return client.ExitOK
	}
	defer conn.Close()
	resp, pingErr := conn.PD.Ping(ctx, &neziav1.PingRequest{})
	if pingErr != nil {
		env.Format.OK(env.Stdout, output.KV{Key: "running", Value: false}, output.KV{Key: "pid", Value: sess.PID})
		return client.ExitOK
	}
	env.Format.OK(env.Stdout,
		output.KV{Key: "running", Value: true},
		output.KV{Key: "pid", Value: sess.PID},
		output.KV{Key: "port", Value: sess.Port},
		output.KV{Key: "version", Value: resp.GetVersion()},
	)
	return client.ExitOK
}

func isNotFound(err error, target **daemonctl.NotFoundError) bool {
	nf, ok := err.(*daemonctl.NotFoundError)
	if ok {
		*target = nf
	}
	return ok
}

func discoverErrToCoded(err error) *client.CodedError {
	switch err.(type) {
	case *daemonctl.NotFoundError:
		return &client.CodedError{Code: "DAEMON_NOT_RUNNING", Msg: err.Error(), Exit: client.ExitConnErr}
	case *daemonctl.AmbiguousError:
		return &client.CodedError{Code: "DAEMON_AMBIGUOUS", Msg: err.Error(), Exit: client.ExitAppErr}
	default:
		return &client.CodedError{Code: "DAEMON_ERROR", Msg: err.Error(), Exit: client.ExitAppErr}
	}
}

func startupTimeout(env *Env) time.Duration {
	if env.Opts.Timeout > 0 {
		return env.Opts.Timeout
	}
	return 10 * time.Second
}
