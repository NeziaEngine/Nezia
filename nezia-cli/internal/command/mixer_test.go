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
	lastMixer         *neziav1.MixerDef
	lastChildren      []*neziav1.BufferId
	lastContainerPlay *neziav1.PlayContainerRequest
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

const clipJSON = `{
  "priority": 32,
  "spatial": {"model": "ATTENUATION_MODEL_LINEAR", "minDistance": 2, "maxDistance": 50,
              "rolloff": 1.0, "dopplerLevel": 0.5},
  "effects": [{"position": "CHAIN_POSITION_PRE", "enabled": true,
               "highPass": {"cutoff": 300, "q": 0.7}}],
  "sends": [{"targetBus": "ReverbBus", "position": "CHAIN_POSITION_POST", "gain": 0.4}]
}`

func TestPlayForwardsClipParams(t *testing.T) {
	path := filepath.Join(t.TempDir(), "clip.json")
	if err := os.WriteFile(path, []byte(clipJSON), 0o644); err != nil {
		t.Fatal(err)
	}
	fake := &mixerDaemon{}
	env, _ := newEnv(t, fake, output.JSON)
	if code := Dispatch(env, "play", []string{"3-1", "--clip", path}); code != 0 {
		t.Fatalf("exit=%d", code)
	}
	clip := fake.lastPlay.GetClip()
	if clip.GetPriority() != 32 {
		t.Errorf("priority=%d", clip.GetPriority())
	}
	sp := clip.GetSpatial()
	if sp.GetModel() != neziav1.AttenuationModel_ATTENUATION_MODEL_LINEAR || sp.GetMaxDistance() != 50 {
		t.Errorf("spatial=%v", sp)
	}
	if clip.GetEffects()[0].GetHighPass().GetCutoff() != 300 {
		t.Errorf("effects=%v", clip.GetEffects())
	}
	if clip.GetSends()[0].GetTargetBus() != "ReverbBus" {
		t.Errorf("sends=%v", clip.GetSends())
	}
}

func (m *mixerDaemon) CreateContainer(
	_ context.Context,
	req *neziav1.CreateContainerRequest,
) (*neziav1.CreateContainerResponse, error) {
	m.lastChildren = req.GetChildren()
	return &neziav1.CreateContainerResponse{
		Container: &neziav1.ContainerHandle{Index: 7, Generation: 2},
	}, nil
}

func (m *mixerDaemon) PlayContainer(
	_ context.Context,
	req *neziav1.PlayContainerRequest,
) (*neziav1.PlayResponse, error) {
	m.lastContainerPlay = req
	return &neziav1.PlayResponse{Source: &neziav1.SourceHandle{Index: 9, Generation: 1}}, nil
}

func TestContainerCreateAndPlay(t *testing.T) {
	fake := &mixerDaemon{}
	env, stdout := newEnv(t, fake, output.JSON)
	if code := Dispatch(env, "container", []string{"create", "0-0", "1-0", "2-0"}); code != 0 {
		t.Fatalf("exit=%d", code)
	}
	if len(fake.lastChildren) != 3 || fake.lastChildren[2].GetIndex() != 2 {
		t.Errorf("children=%v", fake.lastChildren)
	}
	if got := stdout.String(); got != `{"ok":true,"container":"7-2"}`+"\n" {
		t.Errorf("stdout=%q", got)
	}

	env2, stdout2 := newEnv(t, fake, output.Text)
	if code := Dispatch(env2, "container", []string{"play", "7-2", "--bus", "SFX", "--volume", "0.5"}); code != 0 {
		t.Fatalf("exit=%d", code)
	}
	if fake.lastContainerPlay.GetBus() != "SFX" || fake.lastContainerPlay.GetVolume() != 0.5 {
		t.Errorf("play=%v", fake.lastContainerPlay)
	}
	if got := stdout2.String(); got != "ok source=9-1\n" {
		t.Errorf("stdout=%q", got)
	}
}
