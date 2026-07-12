package command

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"jp.nezia/nezia-cli/internal/client"
	neziav1 "jp.nezia/nezia-cli/internal/gen/neziav1"
	"jp.nezia/nezia-cli/internal/output"
)

// mixerDaemon は LoadMixer / Play を記録するフェイク。
type mixerDaemon struct {
	fakeDaemon
	lastMixer *neziav1.MixerDef
}

func (m *mixerDaemon) LoadMixer(
	_ context.Context,
	req *neziav1.LoadMixerRequest,
) (*neziav1.LoadMixerResponse, error) {
	m.lastMixer = req.GetMixer()
	var buses []*neziav1.NamedBus
	for i, b := range req.GetMixer().GetBuses() {
		buses = append(buses, &neziav1.NamedBus{
			Name: b.GetName(),
			Bus:  &neziav1.BusHandle{Index: uint32(i + 1), Generation: 1},
		})
	}
	return &neziav1.LoadMixerResponse{Buses: buses}, nil
}

const mixerJSON = `{
  "buses": [
    {"name": "BGM", "gain": 1.0,
     "effects": [{"position": "CHAIN_POSITION_PRE", "enabled": true,
                  "lowPass": {"cutoff": 800, "q": 0.7}}]},
    {"name": "SFX", "gain": 0.9, "muted": true}
  ],
  "sends": [
    {"sourceBus": "SFX", "targetBus": "BGM", "position": "CHAIN_POSITION_POST", "gain": 0.5}
  ]
}`

func TestMixerLoadParsesProtoJSON(t *testing.T) {
	path := filepath.Join(t.TempDir(), "mixer.json")
	if err := os.WriteFile(path, []byte(mixerJSON), 0o644); err != nil {
		t.Fatal(err)
	}
	fake := &mixerDaemon{}
	env, stdout := newEnv(t, fake, output.JSON)
	if code := Dispatch(env, "mixer", []string{"load", path}); code != 0 {
		t.Fatalf("exit=%d stdout=%q", code, stdout.String())
	}
	def := fake.lastMixer
	if len(def.GetBuses()) != 2 || len(def.GetSends()) != 1 {
		t.Fatalf("mixer=%v", def)
	}
	lp := def.GetBuses()[0].GetEffects()[0].GetLowPass()
	if lp.GetCutoff() != 800 || lp.GetQ() != 0.7 {
		t.Errorf("lowpass=%v", lp)
	}
	if def.GetSends()[0].GetTargetBus() != "BGM" {
		t.Errorf("send=%v", def.GetSends()[0])
	}
	if got := stdout.String(); got != `{"ok":true,"buses":{"BGM":"1-1","SFX":"2-1"}}`+"\n" {
		t.Errorf("stdout=%q", got)
	}
}

func TestMixerLoadRejectsBadJSON(t *testing.T) {
	path := filepath.Join(t.TempDir(), "bad.json")
	if err := os.WriteFile(path, []byte(`{"buses": [{"nam`), 0o644); err != nil {
		t.Fatal(err)
	}
	env, stdout := newEnv(t, &mixerDaemon{}, output.JSON)
	if code := Dispatch(env, "mixer", []string{"load", path}); code != client.ExitAppErr {
		t.Fatalf("exit=%d", code)
	}
	if !strings.Contains(stdout.String(), `"code":"INVALID_ARGUMENT"`) {
		t.Errorf("stdout=%q", stdout.String())
	}
}

func TestPlayForwardsBusName(t *testing.T) {
	fake := &mixerDaemon{}
	env, _ := newEnv(t, fake, output.JSON)
	if code := Dispatch(env, "play", []string{"3-1", "--bus", "BGM"}); code != 0 {
		t.Fatalf("exit=%d", code)
	}
	if fake.lastPlay.GetBus() != "BGM" {
		t.Errorf("bus=%q", fake.lastPlay.GetBus())
	}
}
