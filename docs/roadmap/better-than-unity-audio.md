# Roadmap — Better than Unity Audio

NEZIA ENGINE の直近の到達目標は **「Unity 標準 Audio より良い 3D サウンド体験」を NEZIA 単体で提供できる状態** である。
本ドキュメントは、そのために何を・どの順で・なぜ実装するかを定義する。
個別機能の詳細設計は `docs/design/core/spatial.md` 等の設計ドキュメントを正とし、ここでは**順序と判断基準**のみを扱う。

---

## 立ち位置

- ターゲットは Unity プロジェクトの**ドロップイン代替**として機能する 3D サウンドエンジン。
- 競合は Wwise / FMOD / CRI ADX ではなく、まずは **Unity 標準 `AudioSource` + AudioListener** である。
- 「Unity より良い」とは具体的には次の 2 条件を同時に満たすこと:
  1. **Parity（同等機能）**: Unity 標準にある機能はすべて NEZIA にも存在する
  2. **Differentiation（差別化）**: Unity 標準に**無い**機能を NEZIA は持っており、移行する価値がある

差別化機能を派手に増やしても parity に穴があると採用判定の最初で落ちるため、**順序は parity の致命傷を埋める → 差別化** を厳守する。

---

## ギャップ分析

### Unity AudioSource にあって NEZIA に無いもの (parity gap)

| 機能 | 重要度 | 実装コスト | 状態 |
|------|--------|-----------|------|
| Doppler 効果 | 高 | 中 (1〜2 週間) | 未着手 (SP-10) |
| Custom Attenuation Curve (任意減衰カーブ) | 中 | 中 (1〜2 週間) | 未計画 |
| Spread (ステレオ音源の角度依存広がり) | 低 | 小 (数日) | 未計画 |
| Reverb Zone | 中 | 大 (数ヶ月級) | 未着手 (SP-14) |

### Unity AudioSource に無くて NEZIA で勝てるもの (差別化)

| 機能 | 訴求力 | 実装コスト | 状態 |
|------|--------|-----------|------|
| Listener Focus (仮想リスナー) | 中 | — | **実装済 (SP-06)** |
| Sound Cone (指向性音源) | 高 | 小 (数日〜1 週間) | 未着手 (SP-11) |
| Geometry-based Occlusion | 高 | 大 (1〜2 ヶ月) | 未着手 (SP-12) |
| HRTF (バイノーラル定位) | 最高 | 特大 (1〜3 ヶ月) | 未着手 (SP-13) |

### 既に持っているもの (Phase 1 完了分)

- 距離減衰 (Linear / InverseDistance / Exponential) — SP-01, SP-04
- ステレオパンニング (sin-azimuth ベース、後方連続) — SP-02
- リスナー管理 — SP-03
- 空間演算 ON/OFF 切替 — SP-05
- リスナーフォーカス — SP-06

---

## 実装順

```
Phase 1 (完了)
  距離減衰 / パンニング / リスナー / 有効切替 / フォーカス

──────── ここから「Unity 並み」 (Parity) ────────

Step 1. Doppler 効果                       [SP-10]
Step 2. Custom Attenuation Curve            [新規]

──────── ここから「Unity 超え」 (Differentiation) ────────

Step 3. Sound Cone                          [SP-11]
Step 4. Occlusion または HRTF (分岐)        [SP-12 / SP-13]

──────── 後続 ────────

Step 5. Reverb Zone / Spread / 残り
```

### Step 1 — Doppler 効果 (SP-10)

**なぜ最初か。** Unity は標準で Doppler を持っており、`AudioSource.dopplerLevel` で誰でも触れる。
NEZIA に無い状態では「Unity の置き換え」と主張できない。**parity の最大の致命傷**。

**実装の見通し。**
- `SourceWorld` に `velocities_x/y/z: Vec<f32>` を追加 (既存 SoA 拡張)
- `ListenerState` に `velocity: [f32; 3]` を追加
- `BatchSetSourceVelocities` コマンドを `BatchSetSourcePositions` と同形で追加
- `SpatialSystem::compute_gains` の隣に `compute_doppler_pitch` を置き、`SourceMixingSystem` 側で `effective_pitch = source.pitch * doppler_pitch`

構造変更が小さく、既存パターンの完全な再利用で済む。**着手しやすさが最も高い。**

**完了条件。** ドップラーシフトが既存の Unity AudioSource と聴感上区別できないこと（音速 340 m/s デフォルト、`doppler_level` で 0.0〜5.0 にスケール可能）。

### Step 2 — Custom Attenuation Curve

**なぜ Doppler の次か。** Unity の `AudioRolloffMode.Custom` + `AnimationCurve` はサウンドデザイナーの**最大の窓口**であり、これが無いと「カーブを Unity と同じに作りたい」要求に応えられず移行が止まる。

**実装の見通し。**
- `AttenuationModel` に `Custom { curve_id: u32 }` バリアントを追加
- 別ストレージ `CurveBank: Vec<Curve>` を持ち、`Curve` は `Vec<(distance, gain)>` を保持
- ホットループでは二分探索 + 線形補間 (将来テーブル化)
- 既存 3 モデル (Linear / InverseDistance / Exponential) はそのまま残し、破壊的変更なし

**完了条件。** Unity 側でエクスポートしたカーブ点列を NEZIA 側で再現でき、聴感上ほぼ一致。

### Step 3 — Sound Cone (SP-11)

**ここから差別化フェーズ。** Unity 標準に**無い**機能のうち、コスト/インパクト比が最も高い。
拡声器・武器発射方向・キャラの向きで音色が変わる演出が標準で書けるようになり、ミドルウェア感が一気に出る。

**実装の見通し。**
- SoA に `cone_directions_x/y/z`, `cone_inner_angles`, `cone_outer_angles`, `cone_outer_gains` を追加
- API 形は OpenAL / FMOD / Web Audio に揃える (`set_source_cone(entity, direction, inner_angle, outer_angle, outer_gain)`)
- 計算は `dot(cone_direction, normalize(source→listener))` → 内側コーン内/外側コーン外/中間で線形補間
- 既存 `dist_gain * pan_gain` に `cone_gain` を乗算するだけ。ホットループへの追加コストは最小

**Wwise 流のコーン外 LPF までやるかは要検討。** やる場合 API は `outer_lpf_cutoff: Option<f32>` で拡張、デフォルト None。Phase 2 後半に回しても良い。

**完了条件。** 単体機能として動くこと + ドキュメント化 + Unity サンプルでメガホン的な指向性音源を実演できること。

### Step 4 — Occlusion または HRTF (分岐判断)

ここで**プロジェクトのターゲット層によって優先順を分岐**する。両方やるとフェーズが長くなりすぎるので、片方を先に打ち出して短いリリースサイクルで価値を見せる。

#### 4-A. Occlusion 優先 (推奨: モバイル / カジュアル / TPS / FPS)

- 効果が**聴感上わかりやすい**: 壁の向こうでこもる、扉が閉まると遮断されるなど
- レイキャスト or ジオメトリクエリのインターフェース設計が重い (ゲーム側に問い合わせる API)
- HRTF より CPU が軽く、モバイルで現実的

#### 4-B. HRTF 優先 (推奨: VR / コンソール / ヘッドフォン主体)

- 没入感の差が決定的
- Unity は標準で持たず Oculus/MS の別プラグインに依存している弱点を直接突ける
- 実装は重い (1〜3 ヶ月、SOFA ローダー + FFT 畳み込み + 補間)
- `MAX_SOURCES` 全部に HRTF はかけない設計が必須 (近接 N 体だけ HRTF / 残りは現行パン)

**判断は Step 3 完了時点で確定する。** それまでにユーザーフィードバックとターゲットタイトルの方向性が見えているはず。

### Step 5 — Reverb Zone / Spread / 残り

- Reverb Zone (SP-14): 環境演出の決定打。ただしリバーブエフェクト本体の実装が前提条件で重い
- Spread: 軽いので隙間に入れる
- そのほか細かい parity 項目 (Bypass フラグ、Priority、PlayOneShot 互換 API など) は使われ方を見ながら拡充

---

## 各ステップ完了時に主張できる立ち位置

| 完了時点 | 主張できる立ち位置 |
|---------|------------------|
| Phase 1 (現在) | 「3D サウンドの最低限が動く」 — Unity AudioSource の主要機能の半分弱 |
| Step 1 完了 | 「Unity 標準 3D サウンドの主要機能を網羅」 — Doppler が入って parity 8 割 |
| Step 2 完了 | 「**Unity 標準と同等**」 — 既存 Unity プロジェクトの音表現をほぼ再現可能 |
| **Step 3 完了** | 「**Unity 標準より良い**」 — Listener Focus + Sound Cone が乗り、Unity 単体ではできないことが標準で書ける |
| Step 4 完了 | 「Unity + プラグイン 2〜3 個分相当」 — VR/ホラー/環境表現で明確に上 |
| Step 5 以降 | Wwise / FMOD クラスの本格ミドルウェアの土台 |

**「Unity より良い」を最短で名乗れるのは Step 3 完了時点。** ここまでで Phase 1 完了からおよそ 4〜6 週間が見込み。

---

## 設計上のガードレール

- **既存 SoA / SIMD / リングバッファコマンドのパターンを壊さない。** 新機能はすべて既存 ECS 上に SoA 列追加 + コマンド追加で実装する。新しい同期機構は導入しない。
- **`spatial_enabled = false` の経路を常に最速に保つ。** 2D ソース (BGM, UI) は新機能追加で遅くならないこと。
- **Unity 標準にある機能は Unity 互換のデフォルト値で動く。** 新規パラメータは「設定しなければ Unity と同じ挙動」を維持する。
- **API 形はメジャー実装に寄せる。** Sound Cone は OpenAL/FMOD/Web Audio、HRTF は Steam Audio / Resonance Audio の慣習に揃え、学習コストをゼロに近づける。

---

## 関連ドキュメント

- [3D サウンド設計](../design/core/spatial.md) — 個別機能の詳細設計とフェーズ分け
- [統合戦略](../design/integration/CONCEPT.md) — Unity / Unreal とのドロップイン互換方針
- [ECS アーキテクチャ](../design/core/ecs.md) — 新機能を載せる土台
