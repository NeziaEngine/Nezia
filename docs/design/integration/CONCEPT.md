# integration — コンセプト

`integration` は **特定のクレートに対応しない設計領域** で、NEZIA ENGINE を
ゲームエンジン（Unity / Unreal）に組み込む際の「**API 互換戦略**」を定義する。
実装は各エンジン向けの別リポジトリ（`Nezia.Unity`, `Nezia.Unreal` 等）で行うが、
何を提供すべきか・なぜそうするかという指針は core 設計の一部としてここで管理する。

## なぜ「ドロップイン互換」を最優先にするか

サウンドミドルウェアは伝統的に「サウンド専門スタッフが導入し、ゲームプログラマは
専用 API を学習する」というワークフローで普及してきた（Wwise, FMOD など）。
これはプロジェクト後半でしかミドルウェア検討が始まらない原因になり、
「最初から Nezia でモック開発できないか？」という需要に応えにくい。

NEZIA ENGINE の差別化方針は明確に逆を取る:

> **ゲームプログラマがエンジン標準のサウンド API（Unity の `AudioSource` /
> Unreal の `UAudioComponent` 等）を書くだけで Nezia バックエンドが動く。**
> サウンド専門スタッフが本格的なサウンド設計を始める段階で、同じシーン・
> 同じプレハブを Nezia ネイティブ API に段階的に置き換えられる。

これにより:

1. **モック段階から導入できる** — エンジン標準 API しか知らない開発者でも初日から使える。
2. **後付け移行コストが低い** — 既存プロジェクトの `AudioSource` を一括置換するだけで
   Nezia の恩恵（データ指向ミキシング・大規模同時発音・カスタムエフェクト）に乗れる。
3. **段階的移行が可能** — 全シーンを一度に置き換える必要はなく、重要な箇所から
   ネイティブ API に移していける。

## 並立する 2 つのワークフロー

ドロップイン互換は **唯一のワークフロー** ではなく、**もう 1 つのワークフロー（本格
オーサリング）と並立する**。NEZIA ENGINE は最終的に以下 2 経路を両方提供する:

| ワークフロー | 想定ユーザ | 主な体験 |
|---|---|---|
| **A. ドロップイン互換**（本ドキュメントの主題） | ゲームプログラマ・モック開発・既存プロジェクト移行 | エンジン標準 API（`AudioSource` 等）をそのまま使い、バックエンドが Nezia に差し替わる |
| **B. プロジェクトファイル方式** | サウンド専門スタッフ・本格的なサウンド設計 | 専用オーサリングツール上で論理 ID・バス階層・イベント・ランダム化等を設計し、JSON 等のプロジェクトファイルとして出力。ランタイムはこれをロードしてプレイ | 

両者は **同じ `core::SoundEngine` を駆動する**。違うのは「**誰が・どう設定を組み立てるか**」だけで、
オーディオエンジン本体としての挙動は共通。

具体的には:

- **A（ドロップイン）** は C# / Blueprint コードから `nezia_*` ffi を直接呼び、
  ランタイムで動的に Source / Bus を生成する。
- **B（プロジェクトファイル）** はオーサリングツールが事前に論理 ID 体系
  （例: `"sfx/explosion"` → ハッシュ）と Bus ツリーを定義する。
  ランタイムはプロジェクトファイルをロードして物理 ID に解決し、
  ゲームコードは論理 ID で `Play("sfx/explosion")` のように呼ぶ。

両者は混在可能。たとえば「BGM はプロジェクトファイル方式で精密に設計、
SE のうち単発のものはドロップイン API で雑に再生」といった併用ができる。

このため、core API の設計はどちらか一方に肩入れせず、**論理 ID と物理 ID の二層構造**
（[CLAUDE.md](../../../CLAUDE.md) 参照）を保つ。これがプロジェクトファイル方式の
基盤になる。

### 提供順序

実装は A → B の順で進める:

1. **フェーズ 1（直近）**: ドロップイン互換を完成させ、ゲームプログラマが Nezia を
   触り始められる状態にする。
2. **フェーズ 2（後続）**: プロジェクトファイル形式を策定し、専用オーサリングツールと
   ランタイムロード API を提供する。

フェーズ 2 の具体的設計（ファイル形式、オーサリングツール構成、エディタ統合）は
別ドキュメント（`docs/design/authoring/` 等、新設予定）で扱う。

## 互換性のレイヤ構造

ドロップイン互換は単一レイヤではなく、4 段階に分けて設計する。

| レベル | 名称 | 対応物 | 互換度 |
|---|---|---|---|
| 1 | コンポーネント API 互換 | `AudioSource` / `UAudioComponent` の主要メソッド・プロパティ | ソース互換（メンバ名一致） |
| 2 | アセット型互換 | `AudioClip` / `USoundBase` をそのまま受け取れる | アセットパイプライン透過 |
| 3 | シーン透過変換 | エディタ拡張で既存 `AudioSource` を一括置換 | プロジェクト透過 |
| 4 | リスナー互換 | `AudioListener` / `UAudioComponent` 自動追従 | シーングラフ透過 |

各レベルは独立に提供できる。たとえば「レベル 1 と 4 だけ実装してレベル 3 はスキップ」も
ありえる構成。

### レベル 1: コンポーネント API 互換

ゲームプログラマが書くコードを変えないことを目的とする。Unity の場合:

```csharp
// 標準 Unity コード
var src = gameObject.AddComponent<AudioSource>();
src.clip = myClip;
src.volume = 0.8f;
src.pitch = 1.2f;
src.spatialBlend = 1.0f;
src.Play();
```

これと同じシグネチャを持つ `NeziaAudioSource : MonoBehaviour` を提供する。
内部的には Nezia の `nezia_source_*` API を呼ぶが、ユーザーから見ると名前の違いを除き
振る舞いが同じ。Find & Replace で `AudioSource` → `NeziaAudioSource` だけで動く。

互換させるべき主要メンバ（Unity）:

- メソッド: `Play() / PlayDelayed(s) / PlayOneShot(clip) / Stop() / Pause() / UnPause()`
- プロパティ: `clip / volume / pitch / loop / mute / spatialBlend / minDistance / maxDistance / rolloffMode / outputAudioMixerGroup / time / isPlaying`
- 静的: `AudioSource.PlayClipAtPoint(clip, position)`

完全 1:1 互換は目指さない（`reverbZoneMix` 等の高度な項目は対象外）。**80% の使用ケースを
標準コードのままで動かす** ことが目標。

#### 設計判断: 寿命モデルの差異を統合層が吸収する

Unity の `AudioSource` は「永続インスタンスを保持し、`Play()` / `Stop()` を繰り返し
呼ぶ」モデル。一方 NEZIA core の Source は **1 回の発音ごとに新しい EntityId を取り
直す短命ボイス**モデル（[core/source.md](../core/source.md) 参照）。

このインピーダンス差は core を太らせず、統合層で吸収する:

- `NeziaAudioSource` は永続インスタンス側の状態（clip / volume / pitch / loop /
  spatial 設定）を C# 側にキャッシュとして保持する。
- `Play()` のたびに内部で `nezia_source_play_with_handle()` を呼び、新しい EntityId を
  取得して保持中の設定を反映する。前の EntityId は捨てる（Stop 済みなら自動 despawn、
  まだ鳴っていれば従来どおり鳴り続ける = Unity の OneShot 的な挙動）。
- `Stop()` は現在の EntityId に対して `nezia_source_stop()` を発行。次の `Play()` で
  EntityId は新規取得される。
- `isPlaying` は core 側の `is_source_alive()` で問い合わせる。

これにより core は「1 Source = 1 発音」という単純なライフサイクルを保ったまま、
Unity 側のユーザーには「永続インスタンス」体験を提供できる。プロジェクトファイル方式
（B）は逆に core のライフサイクルにそのまま乗るため、ラッパー不要。

### レベル 2: アセット型互換

レベル 1 の体験を成立させるには「Inspector でドラッグして `clip` プロパティに
セットできるアセット型」が必要となる。

#### 設計判断: なぜ AudioClip 継承ではないのか

最も自然な答えは「`NeziaAudioClip : AudioClip`」だが、これは不可能。
`UnityEngine.AudioClip` は `sealed class` で継承できない。代替策の検討:

| 案 | 結論 |
|---|---|
| AudioClip を直接継承 | `sealed` のため不可 |
| `AudioClip.Create(stream: true, pcmReadCallback: ...)` で AudioClip を生成し ScriptedImporter に乗せる | runtime インスタンスのため asset として serialize できない（`.wav` から asset 化するには標準 AudioImporter が必要だが、それを通すと PCM フルロードになりこの設計の意味がない） |
| ScriptedImporter で独自 ScriptableObject を生成 | **採用**。Inspector ドラッグ・Addressables・AssetBundle 全部動く |

→ 独自型 `NeziaAudioClip : ScriptableObject` を採用する。**ただし、必要な時に
`AudioClip.Create(stream: true)` で「PCM 実体を持たない AudioClip」を遅延生成して
返す `AsAudioClip()` メソッドを提供する**ことで、Timeline / 既存コード等の
AudioClip 必須箇所にも対応する。

#### `NeziaAudioClip`（ScriptedImporter 方式）

`.wav .ogg .flac .mp3` を ScriptedImporter で **横取り** し、Unity 標準 AudioClip
ワークフローを完全に置き換える。Nezia 採用プロジェクトでは AudioClip を別途持つ
理由がないため、これがデフォルト動作。

```csharp
[ScriptedImporter(version: 1, exts: new[] { "wav", "ogg", "flac", "mp3" })]
public sealed class NeziaAudioImporter : ScriptedImporter {
    public override void OnImportAsset(AssetImportContext ctx) {
        var bytes = File.ReadAllBytes(ctx.assetPath);
        var info  = NeziaDecoder.PeekHeader(bytes);  // symphonia でヘッダだけ読む

        var clip = ScriptableObject.CreateInstance<NeziaAudioClip>();
        clip.encodedBytes = bytes;
        clip.sampleRate   = info.sampleRate;
        clip.channels     = info.channels;
        clip.totalSamples = info.totalSamples;
        clip.format       = info.format;

        ctx.AddObjectToAsset("main", clip);
        ctx.SetMainObject(clip);
    }
}
```

ユーザコード（Unity 標準と同じ触感）:

```csharp
public NeziaAudioClip clip;          // Inspector でドラッグ
neziaSource.clip = clip;
neziaSource.Play();
```

#### `AsAudioClip()`: AudioClip 必須箇所への橋渡し

Timeline `AudioTrack`、Animation Event、サードパーティアセット等が `AudioClip` 型を
要求するケースは現実に存在する。これらに対しては **`AudioClip.Create(stream: true)`
で PCM 実体を持たない AudioClip façade を遅延生成**する:

```csharp
public sealed class NeziaAudioClip : ScriptableObject {
    [SerializeField] internal byte[] encodedBytes;
    [SerializeField] internal int    sampleRate;
    [SerializeField] internal int    channels;
    [SerializeField] internal int    totalSamples;

    private AudioClip _proxy;

    /// Unity AudioClip が必要な箇所への橋渡し。
    /// PCM 実体は Nezia 側に残り、Unity 側はストリームコールバックで都度受け取る。
    public AudioClip AsAudioClip() {
        if (_proxy == null) {
            _proxy = AudioClip.Create(
                name:           name,
                lengthSamples:  totalSamples,
                channels:       channels,
                frequency:      sampleRate,
                stream:         true,
                pcmReadCallback:        ReadPcmFromNezia,
                pcmSetPositionCallback: SeekPcmInNezia
            );
        }
        return _proxy;
    }
}
```

呼び出し例:

```csharp
// Timeline トラックや既存 AudioSource が必要な箇所
audioSource.clip = neziaClip.AsAudioClip();
audioSource.Play();
```

このとき:
- Unity 側に乗るのは数百サンプル分の DSP リングバッファのみ（〜1KB）
- PCM 実体は Nezia の `nezia_buffer_load_from_memory(encodedBytes)` で確保した側にある
- `pcmReadCallback` は Unity のオーディオスレッドから呼ばれるため lock-free 必須。
  Nezia の SPSC リングバッファ越しに供給する

`AsAudioClip()` の利用は補助手段であり、推奨は `NeziaAudioSource` 経由の直接利用。

#### メリットまとめ

- **メモリ二重持ちなし**。Unity AudioClip 経路を完全に迂回。`AsAudioClip()` 経由でも
  PCM 実体は Nezia 側にしか存在しない
- **Unity の再エンコードを通らない**。元ファイルのビット深度・コーデック・サンプル
  レートがそのまま保たれる
- **Inspector UX を Nezia 独自に拡張できる**。波形プレビュー、ループ点、既定減衰
  モデル、Nezia バス指定などの authoring 情報をアセット自身が持てる
- **Addressables / AssetBundle に普通に乗る**（ScriptableObject）
- **AudioClip 必須箇所にも対応可能**。`AsAudioClip()` で逃げ道を確保

#### 既存 `AudioClip` 資産からの移行ブリッジ

既存プロジェクトを Nezia に移行する **過渡期のみ** 使う互換 API として
`LoadFromAudioClip` を提供する:

```csharp
async Task<NeziaBuffer> Nezia.Buffer.LoadFromAudioClip(AudioClip clip) {
    EnsureLoaded(clip);                    // LoadAudioData() + loadState ポーリング
    var pcm = new float[clip.samples * clip.channels];
    clip.GetData(pcm, 0);
    var buf = nezia_buffer_load_from_pcm(pcm, clip.channels, clip.frequency);
    clip.UnloadAudioData();                // ← Unity 側 PCM を即時解放
    return buf;
}
```

`AudioClip.LoadAudioData()` / `UnloadAudioData()` で Unity 側 PCM を即解放することで
メモリ二重持ちを回避する。これは「Nezia 側に再保存できない既存 AudioClip 資産の
延命」にのみ用いる暫定 API。新規プロジェクトでは使用しない。

**制約事項**:

- `loadType = Streaming`: `GetData()` で全 PCM が得られない。`NotSupportedException`
  を投げ、元ファイルから `NeziaAudioClip` への再 import を案内する
- `loadInBackground = true`: `LoadAudioData()` が非同期。`loadState` を `Loaded` まで
  ポーリングする
- Unity 側で再エンコードされた PCM が経由するため、作家の意図と細部がずれる可能性
  がある（**長期的には `NeziaAudioClip` への移行を推奨**）

### レベル 3: シーン透過変換

Unity Editor 拡張として、選択中の GameObject 配下の `AudioSource` を再帰的に
`NeziaAudioSource` に置き換える `Tools > Nezia > Replace AudioSources` メニューを提供する。

逆方向の変換（Nezia → AudioSource）も用意することで、Nezia バックエンドを一時的に
無効化してデバッグする運用にも対応する。

### レベル 4: リスナー互換

シーン内の `AudioListener` コンポーネントを自動検出し、そのトランスフォームから
`nezia_listener_set()` を毎フレーム呼ぶ橋渡しコンポーネント（`NeziaListenerBridge`）を
提供する。ユーザーは `AudioListener` をそのままシーンに置けばよく、Nezia 固有の
リスナー設定を意識しない。

## core / ffi 側に必要な API 追加

ドロップイン互換を成立させるため、core API にいくつか追加が必要となる。

| API | 目的 | 対応するレベル |
|---|---|---|
| `SoundEngine::load_from_memory(bytes: &[u8]) -> Result<BufferId>` | `NeziaAudioClip.encodedBytes` のロード（メイン経路）、Resources / Addressables / WebRequest 等のバイト列ロード | 推奨ワークフロー全般 |
| `SoundEngine::load_from_pcm(samples: &[f32], channels: u32, sample_rate: u32) -> BufferId` | `AudioClip.GetData()` 結果を直アップロード（移行期間用） | レベル 2 ブリッジ |
| 既存 `SoundEngine::load(path)` | ファイルパス直指定（desktop でのデバッグ用） | 補助 |

ffi 側ではそれぞれ `nezia_buffer_load_from_memory` / `nezia_buffer_load_from_pcm` として
公開する。引数規約は `*const T` + `usize` の素直な配列渡し。

UE 互換のために追加 API が必要かは Unreal 統合の設計時に再評価する。

## Integration 層が吸収する責務

core / ffi の API を最小に保つため、以下の責務は **Integration 層側で実装**する。core にはこれらに対応する API を追加しない。

### 個別 API 感覚 ⇄ バッチ呼び出しの変換

core の `nezia_source_batch_set_positions` は配列を 1 回で受け取る形（triple buffer 経由・tearing なし・alloc 0）。一方 Unity ユーザは `audioSource.transform.position = v` のような **個別書き込みの感覚** で扱いたい。

この差は Integration 層が吸収する:

- `NeziaAudioSource` ラッパが各ソースの desired position を C# 側に保持
- フレーム末尾（`LateUpdate` 等）に全ソースぶんを配列化して `nezia_source_batch_set_positions` を 1 回呼ぶ
- ユーザコードからは「個別代入の感覚」、core から見ると「バッチ呼び出し」

この棲み分けにより:

- core は「個別 position API」を持たなくて済む（経路が増えない、両 API 混在時の挙動仕様も増えない）
- 既存の triple buffer 経路の利点（一貫スナップショット・alloc 0・レイテンシ最短）をそのまま享受
- volume / pitch / spatial_enabled は **個別 API のまま** Atomic per-slot で受ける（[threading.md](../core/threading.md) 参照）。これらはスカラーで `AtomicU64` 1 個に収まり tearing しないため core に直接個別 API がある

### 高レベル抽象（FMOD Event 相当・サウンドバンク等）

これらも core の責務外。各エンジン向けプラグインで独自に実装する。

## アセットワークフローの推奨順位

| 順位 | 経路 | 用途 |
|---|---|---|
| 1 | `Assets/.../*.wav` 等を Project に置く → ScriptedImporter が `NeziaAudioClip` 化 → `NeziaAudioSource.clip` にドラッグ | **新規プロジェクト・本番ワークフロー** |
| 2 | `Nezia.Buffer.LoadFromBytes(byte[])` | 動的ロード（DLC・WebRequest・Addressables 等で取得したバイト列） |
| 3 | `Nezia.Buffer.LoadFromAudioClip(AudioClip)` | 既存 `AudioClip` 資産の **移行期間のみ** の暫定利用 |

経路 1 が Nezia のすべてのメリット（再エンコード劣化なし・メモリ二重持ちなし・
Unity 標準と同じ触感）を享受できる正規ルート。経路 3 は移行用の橋であり、
本番出荷時には経路 1 への置き換えを完了することを前提とする。

## 非目標

- **Nezia と Unity 標準オーディオを同一プロジェクト内で長期併用するワークフロー**は
  対象外。サウンドミドルウェアの価値（統一バスルーティング・統一ボイス管理・統一
  3D 処理）は全採用してこそ生きるため、Nezia 採用プロジェクトは「Unity 標準オーディオを
  完全に置き換える」前提で設計する。`AudioClip` ブリッジや `AsAudioClip()` は短期の
  移行・例外対応のための橋であり、長期共存を想定した API ではない。
- **Unity AudioMixer の完全互換は対象外**。Nezia は独自の `Bus` ルーティングを持っており、
  AudioMixerGroup を Nezia Bus にマッピングする変換層は提供するが、AudioMixer の
  Snapshot や DSP プラグインは対応しない。
- **DSP エフェクトプラグインの互換は対象外**。Unity の `OnAudioFilterRead` 相当の機能は
  Nezia 独自のエフェクトチェーンで提供する（後日設計）。
- **Wwise / FMOD からの自動移行ツールは対象外**。これらは独自の Event/Bank 概念を
  持ち、機械的変換が困難。互換層は「**エンジン標準 API**」のみを対象とする。
  （Nezia 自身のオーサリングワークフロー＝プロジェクトファイル方式は別途提供する。
  上記「並立する 2 つのワークフロー」を参照）
