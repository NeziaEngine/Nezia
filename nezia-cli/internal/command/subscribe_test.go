package command

import (
	"strings"
	"testing"

	"jp.nezia/nezia-cli/internal/client"
	neziav1 "jp.nezia/nezia-cli/internal/gen/neziav1"
	"jp.nezia/nezia-cli/internal/output"
)

// streamingDaemon は SubscribeEvents で固定イベント列を流して閉じるフェイク。
type streamingDaemon struct {
	fakeDaemon
	events []*neziav1.EngineEvent
}

func (s *streamingDaemon) SubscribeEvents(
	_ *neziav1.SubscribeEventsRequest,
	stream neziav1.PreviewDaemon_SubscribeEventsServer,
) error {
	for _, ev := range s.events {
		if err := stream.Send(ev); err != nil {
			return err
		}
	}
	return nil // ストリームを正常に閉じる → cli 側は EOF で exit 0
}

func TestSubscribeEmitsJSONLAndExitsOnEOF(t *testing.T) {
	fake := &streamingDaemon{
		events: []*neziav1.EngineEvent{
			{Event: &neziav1.EngineEvent_SourceStopped{
				SourceStopped: &neziav1.SourceStoppedEvent{
					Source: &neziav1.SourceHandle{Index: 12, Generation: 4},
				},
			}},
			{Event: &neziav1.EngineEvent_SubscriberLagged{
				SubscriberLagged: &neziav1.SubscriberLaggedEvent{EventsDropped: 7},
			}},
		},
	}
	env, stdout := newEnv(t, fake, output.JSON)
	if code := Dispatch(env, "subscribe", nil); code != client.ExitOK {
		t.Fatalf("exit=%d", code)
	}
	lines := strings.Split(strings.TrimSpace(stdout.String()), "\n")
	if len(lines) != 2 {
		t.Fatalf("expected 2 lines, got %d: %q", len(lines), stdout.String())
	}
	if lines[0] != `{"ok":true,"event":"source_stopped","source":"12-4"}` {
		t.Errorf("line0=%q", lines[0])
	}
	if lines[1] != `{"ok":true,"event":"subscriber_lagged","events_dropped":7}` {
		t.Errorf("line1=%q", lines[1])
	}
}

func TestSubscribeTextFormat(t *testing.T) {
	fake := &streamingDaemon{
		events: []*neziav1.EngineEvent{
			{Event: &neziav1.EngineEvent_PlayFailed{PlayFailed: &neziav1.PlayFailedEvent{}}},
		},
	}
	env, stdout := newEnv(t, fake, output.Text)
	if code := Dispatch(env, "subscribe", nil); code != client.ExitOK {
		t.Fatalf("exit=%d", code)
	}
	if got := stdout.String(); got != "ok event=play_failed\n" {
		t.Errorf("stdout=%q", got)
	}
}
