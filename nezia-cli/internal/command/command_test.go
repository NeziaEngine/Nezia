package command

import (
	"context"
	"net"
	"strings"
	"testing"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/test/bufconn"

	"jp.nezia/nezia-cli/internal/client"
	neziav1 "jp.nezia/nezia-cli/internal/gen/neziav1"
	"jp.nezia/nezia-cli/internal/output"
)

// fakeDaemon は PreviewDaemon のフェイク実装。
type fakeDaemon struct {
	neziav1.UnimplementedPreviewDaemonServer
	lastLoadPath string
	lastPlay     *neziav1.PlayRequest
	stopAccepted bool
}

func (f *fakeDaemon) Ping(context.Context, *neziav1.PingRequest) (*neziav1.PingResponse, error) {
	return &neziav1.PingResponse{Version: "0.2.0"}, nil
}

func (f *fakeDaemon) LoadBuffer(_ context.Context, req *neziav1.LoadBufferRequest) (*neziav1.LoadBufferResponse, error) {
	f.lastLoadPath = req.GetPath()
	return &neziav1.LoadBufferResponse{Buffer: &neziav1.BufferId{Index: 3, Generation: 1}}, nil
}

func (f *fakeDaemon) Play(_ context.Context, req *neziav1.PlayRequest) (*neziav1.PlayResponse, error) {
	f.lastPlay = req
	return &neziav1.PlayResponse{Source: &neziav1.SourceHandle{Index: 12, Generation: 4}}, nil
}

func (f *fakeDaemon) Stop(context.Context, *neziav1.StopRequest) (*neziav1.StopResponse, error) {
	return &neziav1.StopResponse{Accepted: f.stopAccepted}, nil
}

func (f *fakeDaemon) StopAll(context.Context, *neziav1.StopAllRequest) (*neziav1.StopAllResponse, error) {
	return &neziav1.StopAllResponse{Accepted: f.stopAccepted}, nil
}

// newEnv はフェイク daemon に接続する Env とサーバ停止関数を返す。
func newEnv(t *testing.T, fake neziav1.PreviewDaemonServer, format output.Format) (*Env, *strings.Builder) {
	t.Helper()
	lis := bufconn.Listen(1 << 20)
	srv := grpc.NewServer()
	neziav1.RegisterPreviewDaemonServer(srv, fake)
	go func() { _ = srv.Serve(lis) }()
	t.Cleanup(srv.Stop)

	conn, err := grpc.NewClient("passthrough:///bufnet",
		grpc.WithContextDialer(func(ctx context.Context, _ string) (net.Conn, error) {
			return lis.DialContext(ctx)
		}),
		grpc.WithTransportCredentials(insecure.NewCredentials()),
	)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = conn.Close() })

	var stdout strings.Builder
	env := &Env{
		Stdout: &stdout,
		Stderr: &strings.Builder{},
		Format: format,
		DialFunc: func(client.Options) (*client.Client, error) {
			return client.NewWithConn(conn), nil
		},
	}
	return env, &stdout
}

func TestPingJSON(t *testing.T) {
	env, stdout := newEnv(t, &fakeDaemon{}, output.JSON)
	if code := Dispatch(env, "ping", nil); code != 0 {
		t.Fatalf("exit=%d", code)
	}
	if got := stdout.String(); got != `{"ok":true,"version":"0.2.0"}`+"\n" {
		t.Errorf("stdout=%q", got)
	}
}

func TestLoadSendsAbsolutePath(t *testing.T) {
	fake := &fakeDaemon{}
	env, stdout := newEnv(t, fake, output.JSON)
	if code := Dispatch(env, "load", []string{"relative/x.wav"}); code != 0 {
		t.Fatalf("exit=%d", code)
	}
	if !strings.HasPrefix(fake.lastLoadPath, "/") {
		t.Errorf("path not absolute: %q", fake.lastLoadPath)
	}
	if got := stdout.String(); got != `{"ok":true,"buffer":"3-1"}`+"\n" {
		t.Errorf("stdout=%q", got)
	}
}

func TestPlayFlagsAndTextOutput(t *testing.T) {
	fake := &fakeDaemon{}
	env, stdout := newEnv(t, fake, output.Text)
	code := Dispatch(env, "play", []string{"--volume", "0.5", "--pitch", "1.2", "--loop", "3-1"})
	if code != 0 {
		t.Fatalf("exit=%d", code)
	}
	p := fake.lastPlay
	if p.GetBuffer().GetIndex() != 3 || p.GetBuffer().GetGeneration() != 1 ||
		p.GetVolume() != 0.5 || p.GetPitch() != 1.2 || !p.GetLooping() {
		t.Errorf("request=%v", p)
	}
	if got := stdout.String(); got != "ok source=12-4\n" {
		t.Errorf("stdout=%q", got)
	}
}

// フラグは位置引数の後でも解釈される (play 3-1 --volume 0.4)。
func TestPlayFlagsAfterPositional(t *testing.T) {
	fake := &fakeDaemon{}
	env, _ := newEnv(t, fake, output.JSON)
	if code := Dispatch(env, "play", []string{"3-1", "--volume", "0.4"}); code != 0 {
		t.Fatalf("exit=%d", code)
	}
	if fake.lastPlay.GetVolume() != 0.4 {
		t.Errorf("volume=%v", fake.lastPlay.GetVolume())
	}
}

func TestStopRejected(t *testing.T) {
	env, stdout := newEnv(t, &fakeDaemon{stopAccepted: false}, output.JSON)
	if code := Dispatch(env, "stop", []string{"--all"}); code != client.ExitAppErr {
		t.Fatalf("exit=%d, want 1", code)
	}
	if !strings.Contains(stdout.String(), `"code":"REJECTED"`) {
		t.Errorf("stdout=%q", stdout.String())
	}
}

func TestInvalidHandle(t *testing.T) {
	env, stdout := newEnv(t, &fakeDaemon{}, output.JSON)
	if code := Dispatch(env, "play", []string{"banana"}); code != client.ExitAppErr {
		t.Fatalf("exit=%d, want 1", code)
	}
	if !strings.Contains(stdout.String(), `"code":"INVALID_ARGUMENT"`) {
		t.Errorf("stdout=%q", stdout.String())
	}
}

func TestDaemonNotRunningExitCode(t *testing.T) {
	var stdout strings.Builder
	env := &Env{
		Stdout: &stdout,
		Stderr: &strings.Builder{},
		Format: output.JSON,
		DialFunc: func(client.Options) (*client.Client, error) {
			return nil, &client.CodedError{Code: "DAEMON_NOT_RUNNING", Msg: "no port file", Exit: client.ExitConnErr}
		},
	}
	if code := Dispatch(env, "ping", nil); code != client.ExitConnErr {
		t.Fatalf("exit=%d, want 2", code)
	}
	if !strings.Contains(stdout.String(), `"code":"DAEMON_NOT_RUNNING"`) {
		t.Errorf("stdout=%q", stdout.String())
	}
}

// help はトップレベル 40 行以内を維持する (CONCEPT.md §6 のサイズ規律)。
func TestUsageWithin40Lines(t *testing.T) {
	lines := strings.Count(strings.TrimRight(usage, "\n"), "\n") + 1
	if lines > 40 {
		t.Errorf("usage is %d lines, must be <= 40", lines)
	}
}
