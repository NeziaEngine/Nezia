package command

import (
	"bufio"
	"encoding/json"
	"strings"
	"testing"
	"time"

	"jp.nezia/nezia-cli/internal/client"
	"jp.nezia/nezia-cli/internal/daemonctl"
	"jp.nezia/nezia-cli/internal/output"
)

func withFakeDaemonctl(t *testing.T, start func(time.Duration) (daemonctl.Session, error),
	stop func(daemonctl.Session, time.Duration) error,
	discover func(int) (daemonctl.Session, error)) {
	t.Helper()
	origStart, origStop, origDiscover := startFunc, stopFunc, discoverFunc
	if start != nil {
		startFunc = start
	}
	if stop != nil {
		stopFunc = stop
	}
	if discover != nil {
		discoverFunc = discover
	}
	t.Cleanup(func() { startFunc, stopFunc, discoverFunc = origStart, origStop, origDiscover })
}

func plainEnv(format output.Format) (*Env, *strings.Builder) {
	var stdout strings.Builder
	return &Env{Stdout: &stdout, Stderr: &strings.Builder{}, Format: format}, &stdout
}

func TestDaemonStart(t *testing.T) {
	withFakeDaemonctl(t, func(time.Duration) (daemonctl.Session, error) {
		return daemonctl.Session{PID: 4242, Port: 55001}, nil
	}, nil, nil)
	env, stdout := plainEnv(output.JSON)
	if code := Dispatch(env, "daemon", []string{"start"}); code != client.ExitOK {
		t.Fatalf("exit=%d", code)
	}
	if got := stdout.String(); got != `{"ok":true,"pid":4242,"port":55001}`+"\n" {
		t.Errorf("stdout=%q", got)
	}
}

func TestDaemonStopAutoDetect(t *testing.T) {
	var stoppedSess daemonctl.Session
	withFakeDaemonctl(t, nil,
		func(s daemonctl.Session, _ time.Duration) error { stoppedSess = s; return nil },
		func(pid int) (daemonctl.Session, error) { return daemonctl.Session{PID: 4242, Port: 55001}, nil })
	env, stdout := plainEnv(output.JSON)
	if code := Dispatch(env, "daemon", []string{"stop"}); code != client.ExitOK {
		t.Fatalf("exit=%d", code)
	}
	if stoppedSess.PID != 4242 {
		t.Errorf("stopped wrong session: %+v", stoppedSess)
	}
	if got := stdout.String(); got != `{"ok":true,"pid":4242}`+"\n" {
		t.Errorf("stdout=%q", got)
	}
}

func TestDaemonStopAmbiguous(t *testing.T) {
	withFakeDaemonctl(t, nil, nil, func(int) (daemonctl.Session, error) {
		return daemonctl.Session{}, &daemonctl.AmbiguousError{Candidates: []int{1, 2}}
	})
	env, stdout := plainEnv(output.JSON)
	if code := Dispatch(env, "daemon", []string{"stop"}); code != client.ExitAppErr {
		t.Fatalf("exit=%d", code)
	}
	if !strings.Contains(stdout.String(), `"code":"DAEMON_AMBIGUOUS"`) {
		t.Errorf("stdout=%q", stdout.String())
	}
}

func TestDaemonStatusNotRunning(t *testing.T) {
	withFakeDaemonctl(t, nil, nil, func(int) (daemonctl.Session, error) {
		return daemonctl.Session{}, &daemonctl.NotFoundError{Detail: "no port file"}
	})
	env, stdout := plainEnv(output.JSON)
	if code := Dispatch(env, "daemon", []string{"status"}); code != client.ExitOK {
		t.Fatalf("exit=%d", code)
	}
	if got := stdout.String(); got != `{"ok":true,"running":false}`+"\n" {
		t.Errorf("stdout=%q", got)
	}
}

func TestBatchReusesConnection(t *testing.T) {
	fake := &fakeDaemon{stopAccepted: true}
	env, stdout := newEnv(t, fake, output.JSON)
	stdin := strings.NewReader("load /a.wav\nplay 3-1 --volume 0.4\nstop --all\n")
	code := cmdBatch(env, bufio.NewScanner(stdin))
	if code != client.ExitOK {
		t.Fatalf("exit=%d", code)
	}
	lines := strings.Split(strings.TrimSpace(stdout.String()), "\n")
	if len(lines) != 3 {
		t.Fatalf("expected 3 result lines, got %d: %q", len(lines), stdout.String())
	}
	if !strings.Contains(lines[0], `"buffer":"3-1"`) || !strings.Contains(lines[1], `"source":"12-4"`) {
		t.Errorf("unexpected batch output: %v", lines)
	}
}

func TestSchemaIsValidJSON(t *testing.T) {
	env, stdout := plainEnv(output.Text) // -o text は schema に影響しないはず
	if code := Dispatch(env, "schema", nil); code != client.ExitOK {
		t.Fatalf("exit=%d", code)
	}
	var doc map[string]any
	if err := json.Unmarshal([]byte(stdout.String()), &doc); err != nil {
		t.Fatalf("schema output is not valid JSON: %v\n%s", err, stdout.String())
	}
	if _, ok := doc["commands"]; !ok {
		t.Error("schema missing commands field")
	}
}
