package command

import (
	"context"
	"errors"
	"io"

	"jp.nezia/nezia-cli/internal/client"
	neziav1 "jp.nezia/nezia-cli/internal/gen/neziav1"
	"jp.nezia/nezia-cli/internal/output"
)

// cmdSubscribe はエンジンイベントを 1 イベント 1 行 (JSONL) で stdout に流し続ける。
// daemon が終了するかクライアントが切断されるまでブロックする。
// --timeout はストリームには適用しない (購読は長寿命が前提のため)。
func cmdSubscribe(env *Env) int {
	c, err := env.dial()
	if err != nil {
		return env.fail(err)
	}
	// 長寿命ストリームなので Options.Context (デフォルト 10s) は使わない。
	ctx := context.Background()
	stream, err := c.PD.SubscribeEvents(ctx, &neziav1.SubscribeEventsRequest{})
	if err != nil {
		return env.fail(client.MapRPCError(err))
	}
	for {
		ev, err := stream.Recv()
		if err != nil {
			// daemon 側のストリーム終了 (daemon 停止など) は正常終了として扱う。
			if errors.Is(err, io.EOF) {
				return client.ExitOK
			}
			return env.fail(client.MapRPCError(err))
		}
		emitEvent(env, ev)
	}
}

// emitEvent は EngineEvent を 1 行で出力する。イベント種別は "event" キーで表す。
func emitEvent(env *Env, ev *neziav1.EngineEvent) {
	switch e := ev.GetEvent().(type) {
	case *neziav1.EngineEvent_SourceStopped:
		s := e.SourceStopped.GetSource()
		h := client.Handle{Index: s.GetIndex(), Generation: s.GetGeneration()}
		env.Format.OK(env.Stdout,
			output.KV{Key: "event", Value: "source_stopped"},
			output.KV{Key: "source", Value: h.String()},
		)
	case *neziav1.EngineEvent_PlayFailed:
		env.Format.OK(env.Stdout, output.KV{Key: "event", Value: "play_failed"})
	case *neziav1.EngineEvent_StreamingUnderrun:
		b := e.StreamingUnderrun.GetBuffer()
		h := client.Handle{Index: b.GetIndex(), Generation: b.GetGeneration()}
		env.Format.OK(env.Stdout,
			output.KV{Key: "event", Value: "streaming_underrun"},
			output.KV{Key: "buffer", Value: h.String()},
		)
	case *neziav1.EngineEvent_CaptureOverflow:
		env.Format.OK(env.Stdout,
			output.KV{Key: "event", Value: "capture_overflow"},
			output.KV{Key: "dropped_samples", Value: e.CaptureOverflow.GetDroppedSamples()},
		)
	case *neziav1.EngineEvent_SubscriberLagged:
		env.Format.OK(env.Stdout,
			output.KV{Key: "event", Value: "subscriber_lagged"},
			output.KV{Key: "events_dropped", Value: e.SubscriberLagged.GetEventsDropped()},
		)
	default:
		// 未知のイベント種別 (新しい daemon + 古い cli)。種別だけ通知して継続する。
		env.Format.OK(env.Stdout, output.KV{Key: "event", Value: "unknown"})
	}
}
