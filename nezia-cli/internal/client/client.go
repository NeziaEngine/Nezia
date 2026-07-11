// Package client は daemon への gRPC 接続を担う。
//
// port discovery (docs/design/daemon/CONCEPT.md §2):
// daemon は `$TMPDIR/nezia-daemon-{key}.port` に listen port を書く。
// key はセッション所有者の parent PID、スタンドアロン起動時は daemon 自身の PID。
package client

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"

	"google.golang.org/grpc/codes"
	neziav1 "jp.nezia/nezia-cli/internal/gen/neziav1"
)

// Exit code 規約 (docs/design/cli/CONCEPT.md §5)。
const (
	ExitOK      = 0
	ExitAppErr  = 1 // daemon がエラーを返した / 引数不正
	ExitConnErr = 2 // daemon 未起動 / port discovery 失敗
)

// CodedError は安定したエラーコード enum と exit code を運ぶ。
type CodedError struct {
	Code string
	Msg  string
	Exit int
}

func (e *CodedError) Error() string { return e.Code + ": " + e.Msg }

func connErr(code, format string, a ...any) *CodedError {
	return &CodedError{Code: code, Msg: fmt.Sprintf(format, a...), Exit: ExitConnErr}
}

func appErr(code, format string, a ...any) *CodedError {
	return &CodedError{Code: code, Msg: fmt.Sprintf(format, a...), Exit: ExitAppErr}
}

// Options は接続先の解決方法。優先順: Port 直指定 > ParentPID の port file > glob。
type Options struct {
	Port      int
	ParentPID int
	Timeout   time.Duration
}

type Client struct {
	conn *grpc.ClientConn
	PD   neziav1.PreviewDaemonClient
}

func (c *Client) Close() { _ = c.conn.Close() }

// NewWithConn は確立済みの接続からクライアントを作る (テスト・bufconn 用)。
func NewWithConn(conn *grpc.ClientConn) *Client {
	return &Client{conn: conn, PD: neziav1.NewPreviewDaemonClient(conn)}
}

// Dial は port discovery を行い daemon に接続する。
func Dial(opts Options) (*Client, error) {
	port := opts.Port
	if port == 0 {
		p, err := discoverPort(opts.ParentPID)
		if err != nil {
			return nil, err
		}
		port = p
	}
	return DialAddr(port)
}

// DialAddr は port を直接指定して daemon に接続する (daemonctl の status 確認等で使う)。
func DialAddr(port int) (*Client, error) {
	conn, err := grpc.NewClient(
		fmt.Sprintf("127.0.0.1:%d", port),
		grpc.WithTransportCredentials(insecure.NewCredentials()),
	)
	if err != nil {
		return nil, connErr("CONNECT_FAILED", "dial 127.0.0.1:%d: %v", port, err)
	}
	return &Client{conn: conn, PD: neziav1.NewPreviewDaemonClient(conn)}, nil
}

// discoverPort は port discovery file を読んで接続先 port を返す。
func discoverPort(parentPID int) (int, error) {
	if parentPID == 0 {
		if env := os.Getenv("NEZIA_DAEMON_PID"); env != "" {
			p, err := strconv.Atoi(env)
			if err != nil {
				return 0, connErr("INVALID_ARGUMENT", "NEZIA_DAEMON_PID=%q is not a pid", env)
			}
			parentPID = p
		}
	}
	if parentPID != 0 {
		return readPortFile(portFilePath(parentPID))
	}
	// キー未指定: glob で候補を探す。1 個なら採用、複数なら曖昧エラー。
	matches, _ := filepath.Glob(filepath.Join(os.TempDir(), "nezia-daemon-*.port"))
	switch len(matches) {
	case 0:
		return 0, connErr("DAEMON_NOT_RUNNING", "no port file in %s (start daemon or pass --port)", os.TempDir())
	case 1:
		return readPortFile(matches[0])
	default:
		return 0, connErr("DAEMON_AMBIGUOUS", "multiple daemons: %s (pass --parent-pid or --port)", strings.Join(matches, ", "))
	}
}

func portFilePath(key int) string {
	return filepath.Join(os.TempDir(), fmt.Sprintf("nezia-daemon-%d.port", key))
}

func readPortFile(path string) (int, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return 0, connErr("DAEMON_NOT_RUNNING", "port file %s: %v", path, err)
	}
	port, err := strconv.Atoi(strings.TrimSpace(string(b)))
	if err != nil || port <= 0 || port > 65535 {
		return 0, connErr("DAEMON_NOT_RUNNING", "port file %s has invalid content %q", path, strings.TrimSpace(string(b)))
	}
	return port, nil
}

// Context は RPC 用のタイムアウト付き context を返す。
func (o Options) Context() (context.Context, context.CancelFunc) {
	t := o.Timeout
	if t <= 0 {
		t = 10 * time.Second
	}
	return context.WithTimeout(context.Background(), t)
}

// MapRPCError は gRPC status を安定したエラーコードに正規化する。
func MapRPCError(err error) *CodedError {
	st, ok := status.FromError(err)
	if !ok {
		return appErr("DAEMON_ERROR", "%v", err)
	}
	switch st.Code() {
	case codes.Unavailable:
		return connErr("DAEMON_NOT_RUNNING", "daemon unreachable: %s", st.Message())
	case codes.DeadlineExceeded:
		return connErr("TIMEOUT", "rpc timed out: %s", st.Message())
	case codes.InvalidArgument:
		return appErr("INVALID_ARGUMENT", "%s", st.Message())
	case codes.NotFound:
		return appErr("INVALID_HANDLE", "%s", st.Message())
	case codes.ResourceExhausted:
		// daemon は無効な buffer / voice 上限到達をこのコードで返す。
		return appErr("REJECTED", "%s", st.Message())
	default:
		return appErr("DAEMON_ERROR", "%s: %s", st.Code(), st.Message())
	}
}

// Handle は BufferId / SourceHandle 共通の `<index>-<generation>` 文字列表現。
type Handle struct {
	Index      uint32
	Generation uint32
}

func (h Handle) String() string { return fmt.Sprintf("%d-%d", h.Index, h.Generation) }

// ParseHandle は "3-1" 形式を解釈する。
func ParseHandle(s string) (Handle, error) {
	idx, gen, ok := strings.Cut(s, "-")
	if ok {
		i, err1 := strconv.ParseUint(idx, 10, 32)
		g, err2 := strconv.ParseUint(gen, 10, 32)
		if err1 == nil && err2 == nil {
			return Handle{Index: uint32(i), Generation: uint32(g)}, nil
		}
	}
	return Handle{}, appErr("INVALID_ARGUMENT", "invalid handle %q (expected <index>-<generation>, e.g. 3-1)", s)
}
