<!-- Languages: [English](README.md) · [繁體中文](README.zh-TW.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [한국어](README.ko.md) -->

# MUR Deep Research — 詳細設計

> MUR フリートがエンドツーエンドで所有するネイティブ深層リサーチ。
> **v2.45.0**(2026-07-10)でリリース、PR #663–#672。

サンドボックス化された MUR エージェントの分隊が問いを分解し、単一の監査付きゲートウェイ経由でライブ Web をリサーチし、各主張を敵対的に検証し、**引用付き・暗号学的に帰属可能なレポート**へ収束する — ホストのサブエージェントではなく、MUR 自身のオーケストレーション上で。

- **仕様:** `docs/superpowers/specs/2026-07-09-mur-native-deep-research-design.md`
- **クレート:** `mur-research-gateway` · **コマンド:** `mur-core/src/cmd/deep_research`

---

## §1 · なぜホストのサブエージェントではなくネイティブか

Claude Code 組み込みの deep-research は十分に良くリサーチする — だがそれは *ホストのサブエージェント* として動く。作業は Claude のものであり、MUR はラベルを貼るだけ。ネイティブなオーケストレーションこそが成果を *MUR のもの* にし、かつ証明可能にする。

- **MUR のオーケストレーションが所有する。** ルーターの反復ごとの動的 DAG(`cmd/fleet/plan.rs`)に駆動される MUR エージェントのフリートが、実際にリサーチを行う主体である — ホストが spawn して忘れたサブエージェントではない。
- **暗号学的プロヴェナンス。** すべての主張は、それを生み出したワーカーによって Ed25519 署名付きチャネルへ書き込まれる(Unified Channel v3d-2、*peer-writes-own*)。「このエージェントがこれを見つけた」が検証可能になり、単なる注記ではなくなる。
- **プラットフォーム統合。** 実測の per-token 予算、キルスイッチ、Commander ガバナンス、スケジューリング、長期メモリ — すべて既存のフリート/エージェント機構に無償で乗る。

**非目標:** 組み込みより並列化すること。並行度はどちらも上限がある(約 `min(16, cores−2)`)。ネイティブが勝つのは所有権・プロヴェナンス・ガバナンスであり、生の速度ではない。

---

## §2 · アーキテクチャ — 3 層、1 つのチョークポイント

ワーカーは egress を **持たず**、唯一のインジェクション面である。決定論的で LLM を持たないゲートウェイのみが Web に到達する — しかも強制されたカーネルサンドボックスの内側から。

```
┌─────────────────────── fleet "deep-research" ───────────────────────┐
│  router (mur)  — 反復ごとの動的 DAG (plan.rs)                       │
│  done_when: marker:RESEARCH_COMPLETE · budget-usd · deadline · kill  │
└──────────────────────────────┬──────────────────────────────────────┘
             │ channel/delegate（Ed25519 署名済み返信）
             ▼
┌──────────────── worker × k （entitlements: restricted） ────────────┐
│  model_ref → 実 LLM · MCP を 1 つマウント: research-gateway        │
│  ツール: search · fetch      組み込みツールは全て deny             │
└──────────────────────────────┬──────────────────────────────────────┘
             │ MCP: search(query) / fetch(url)   — 読み取り専用の動詞
             ▼
┌────────── research-gateway（Rust、LLM なし、broad-audited egress）──┐
│  SSRF guard · tier ladder 1→2→3 · content budget · 呼び出しごとの audit │
└──────────────────────────────────────────────────────────────────────┘
       強制対象: B1 カーネルサンドボックス + ループバック egress プロキシ
```

プロンプトインジェクションされたワーカーができるのは、せいぜいゲートウェイに URL を `fetch` させることだけ — ログ化され、SSRF ガードされ、任意ホストへ任意データを POST することはできない。API は `fetch` であって「ソケットを開く」ではない。

---

## §3 · 動的フロー — decompose → research → verify → synthesize

ルーターは `mur fleet run --loop` の反復ごとに新しい DAG を出力する。サブ問いの数は decompose 時に決まる。収束マーカーが単独行に現れるまでループは回る。

1. **Decompose** — ルーターが問いをサブ問い(100+ になり得る)に分解し、作業キューとしてチャネルへ書く。
2. **Research(×N 反復)** — ルーターがバッチをワーカーへ割り当てる(`max_concurrency` で制限)。各ワーカーは `search()`・`fetch()` し、**それぞれ URL + 裏付けの引用に紐づいた主張**を抽出して署名付き返信で書き戻す。キューが尽きるまで繰り返す。
3. **Verify(3 票の敵対的検証)** — 各主張を 3 ワーカーへ、それぞれ異なる反証レンズ(正確性 / ソース独立性 / 新しさ)で配る。2/3 の確認で主張は生き残り、そうでなければ破棄(フェイルセーフ = 破棄)。
4. **Synthesize → 収束** — ルーターが確認済み主張を引用付きレポートへまとめ、単独行に `RESEARCH_COMPLETE` を出力する。構造化された `done_when: marker:…` が決定論的に収束する — 追加の LLM 呼び出しは不要で、マーカーを引用しただけの散文では誤収束しない。

**無償で継承:** *実測* の per-token 会計を伴う `--budget-usd` · `mur fleet stop` キルスイッチ · Commander の kill/budget フック(fail-closed)· 主張ごとの署名済みチャネルプロヴェナンス · 反復上限 / デッドライン / スタック検知のガード。

---

## §4 · research-gateway

MUR に同梱される、依存の軽い小さな Rust MCP サーバー。2 つの読み取り専用動詞、固定でコード駆動の tier ladder(tier を LLM は決めない)、egress の 1 バイトも統治される。

```
search(query, limit?)  →  [{title, url, snippet}]
fetch(url, render?)    →  {url, status, title, text, tier}
```

### エスカレーションの段階(スキルではなく決定論的コード)

| Tier | エンジン | 備考 |
|------|---------|------|
| **1 · http** | `reqwest` GET | デフォルトで最も安い。**Search もここを通る:** DuckDuckGo のサーバーレンダリング HTML エンドポイントを、`fetch` と同じプロキシ尊重パスで GET する(ブラウザ風 User-Agent が必要、無いと DDG は HTTP 202 を返す)。したがってブラウザを spawn できないサンドボックス下でも search は動く。 |
| **2 · lightpanda** | `agent-browser --engine lightpanda` | JS レンダリングページ。`--args ""` は必須 — Chrome の stealth フラグは Lightpanda を壊す。fetch ごとに一意の `--session` id を使い、並行 fetch が cookie jar を共有しない。 |
| **3 · chrome** | `agent-browser --engine chrome` | アンチボット / スクリーンショット。stealth フラグは単一の `--args "<カンマ区切り>"` 値で渡す(裸の argv はサブコマンドとして解釈される)。レンダリング `fetch` は `Http` 失敗 *または* 空レンダリング時に lightpanda → chrome へエスカレートする。`chrome:true` は tier 3 を強制。 |

- **SSRF ガード(強制・設定不可)** — 解決 IP が private / link-local / loopback / unique-local の URL を拒否し、全 tier でスクリーニングする。ブラウザ tier ではガードと `deny_hosts` を **spawn 前のゲートウェイコードで** 強制する — プロキシはブラウザ子プロセス自身の接続を見られない。
- **コンテンツ予算** — 単一の 5 MB ページはワーカーのコンテキストを溢れさせる。`fetch` は返すテキストを `max_fetch_chars`(デフォルト 50 000、`0` で無効)で、コードポイント境界で切り、マーカーを付ける。5 MB のボディ上限は転送/メモリを、`max_fetch_chars` はコンテキストを制限する。search のスニペットは切らない。
- **検索の信頼性** — N 並行ワーカー下で DDG は 202 チャレンジでレート制限する。`search` は 202 時に指数バックオフ + **クエリ由来のジッター** で再試行する — 異なるサブ問いが再試行をずらし、同期して再バーストしない。
- **URL レベル監査** — 各呼び出しは `{worker, url, tier, outcome}` をチャネルとテレメトリへ記録する。レポートの各引用は 1 つのゲートウェイ監査レコードに突き合わせられる。

---

## §5 · セキュリティモデル — サンドボックス、同意、ツールポリシー

ゲートウェイは強制されたカーネルサンドボックス下で、デフォルト egress なしで動く。アクセスはちょうど 1 つの明示的な同意ステップで付与され、ワーカーのツールポリシーは、ヘッドレスなターンが逃げも詰まりもしないよう形作られる。

| 制御 | 仕組み | 境界 |
|------|--------|------|
| **Egress 付与** | ワーカーごとに `mcp set-network research-gateway --broad-audited` — 1 回のオペレーター同意、`EgressAuthorization` として記録。 | フリート作成が egress を暗黙に開くことは **決してない**。「リサーチ用フリートだから」は同意ではない。 |
| **Egress プロキシ** | 1 つのループバック CONNECT プロキシ。各ワーカーのゲートウェイ子は、その allow/deny ポリシーにスコープされた `HTTPS_PROXY` トークンを受け取る。 | サンドボックスが封をする **前** に起動し、そのポートをループバック限定ルールとしてプロファイルへ刻む必要がある — さもないと子はダイヤルできない。 |
| `mcp__research-gateway__*` | **allow** | 事前承認され、ヘッドレスのフリートターンが HITL ゲート(応答者なし → 300 秒タイムアウト → 失敗)を回避する。これ自体は egress を付与しない。 |
| `bash` · `read_file` · `write_file` · `edit_file` | **deny** | リサーチターンが組み込みツールに手を伸ばすと、同じ応答不能なゲートで詰む。deny → 広告されない → 決して呼ばれない。 |
| その他すべて | **ask** | 上記 2 ルール外のツールは fail-closed デフォルトを維持。 |
| **プロヴェナンス** | 各ワーカーは自身の返信を `Agent{self}` イベントとして書き込み、Ed25519 署名(v3d-2 peer-writes-own)、fold 時にアクターごとに検証。 | ルーターはもうワーカーの代わりに署名しない — 帰属はワーカー自身の鍵。 |
| **エクスポート安全性** | `.fleet` インポートは broad-audited を `inherit` へ降格し、承認をクリアする。 | 共有された deep-research フリートは、ローカルで再付与するまで egress ゼロ。 |

**アドバイザリ強制の正直さ。** Tier 1 はプロキシを尊重する。tier 2/3 のブラウザ子プロセスは尊重しないかもしれない — 全域のゲートウェイ URL 監査で緩和し、誇張せず文書化している。完全な封じ込め = 将来の Phase-3 sbpl pin-to-proxy であり、その時ピン留めするのはこの 1 ゲートウェイのみ。

---

## §6 · 運用 — provision、grant、run

```bash
# 1 · k 個の restricted ワーカーを作成し、各々ゲートウェイをマウントし、
#     同じ同意ステップで broad-audited egress を付与する
mur deep-research provision --count 4 --model claude_haiku --grant-egress --yes
#   tool policy: mcp__research-gateway__* → allow
#   tool policy: bash, read_file, write_file, edit_file → deny
#   Updated egress policy for 'research-gateway'.   # × 各ワーカー

# 2 · フリートを作成(router = mur)、done マーカー + 予算を設定
mur fleet create deep-research \
    --members dr_worker_1,dr_worker_2,dr_worker_3,dr_worker_4 \
    --goal "…あなたのリサーチ質問…"
#   fleet.yaml loop: { max_iterations, budget_usd, done_when: marker:RESEARCH_COMPLETE }

# 3 · ガード付きループを実行 — decompose → research → verify → synthesize
mur deep-research run deep-research --max-iterations 4 --deadline 30m
#   fleet 'deep-research' loop stopped after 1 iteration (~$6.14 spent): Converged
```

- **結果の在り処** — 各ワーカーの引用付き返信は `~/.mur/channels/fleet-deep-research/events.jsonl` の署名済みイベント。`~/.mur/index/channels/` の SQLite read-model は再構築可能な射影。各引用は 1 つのゲートウェイ監査レコードに突き合わせられる。
- **設定ノブ(ハードコード値なし)** — `MUR_RESEARCH_MAX_FETCH_CHARS`、`…_SEARCH_LIMIT`、`…_TIMEOUT_SECS`、`…_LIGHTPANDA_PATH`、`…_DENY_HOSTS` — env または `~/.mur/config.yaml` の `research_gateway:`。リテラルは決して使わない。

---

## §7 · 実装の現実 — クリーンな設計を現実にする代償

仕様の 4 ボックスのアーキテクチャは正しかった。しかしフリートを実際に収束させる — サンドボックス化・ヘッドレス・暗号学的帰属付き — には、コードを読むのではなくライブのオペレーター検証で見つかった、9 つの独立した修正が浮かび上がった。

| PR | 領域 | 修正 |
|----|------|------|
| **#663** | feat | **Native deep-research コア** — ゲートウェイクレート、フリート配線、router/worker/verify スキル。 |
| **#664** | HITL | **ゲートウェイツールの事前承認** — ヘッドレスターンには `tool/approval_needed` に答える者がおらず → 300 秒タイムアウト → 失敗。provision 時に `mcp__research-gateway__* → allow` を刻む。 |
| **#665** | G1 · sandbox | **egress プロキシを封の前に起動** — プロキシはサンドボックスが封をした *後* に、決して刻まれないポートで起動していた → すべてのスコープ付き付与が到着即死。封の前に起動し、ループバック限定のポートルールを刻む。 |
| **#666** | G3 · channel | **channel read-model ディレクトリを付与** — 署名済み返信は `events.jsonl` に着地したが SQLite 更新が読み取り専用 DB に当たる → 偽の失敗。`index/channels` を付与し、追記後の更新を非致命に。 |
| **#667** | G2 · search | **ブラウザレス検索 + 動く chrome** — search はサンドボックスが拒む `agent-browser` を spawn していた。tier-1 HTTP へ回す。chrome の `--args` 渡しを直し、レンダリングフォールバックが実際に起動するように。 |
| **#668** | egress | **`Proxy-Authorization` を大文字小文字非依存に** — *egress 全体の解錠*。プロキシは auth ヘッダを大文字小文字依存で照合していたが hyper は小文字で送る → トークン脱落 → *すべての* CONNECT 拒否。`nc` のプロキシトレースでライブ捕捉。 |
| **#669** | content | **Fetch コンテンツ予算** — 単一の 5 MB ページがワーカーのコンテキストを溢れさせた(`anthropic 400: prompt too long`)。返すテキストを `max_fetch_chars` で切る。 |
| **#670** | convergence | **ワーカーの組み込みツールを deny** — *収束の解錠*。リサーチターンが `bash`(デフォルト `ask`)に手を伸ばす → 応答不能な HITL ゲート → ターン失敗 → 空の返信 → ステップ失敗。組み込みを deny すればモデルはそれらを見ることさえない。raw `channel/delegate` ソケットプローブで根本原因を特定。 |
| **#671** | reliability | **DDG 202 の再試行 + ジッター** — 並行ワーカーが DuckDuckGo のレート制限を踏んだ。202 をバックオフ + クエリ由来のジッターで再試行。レポートは「アクセス制限」から各 16–19 引用へ。 |
| **#672** | G4 · cleanup | **ロード時に非スキルディレクトリをスキップ** — `skills/` 下のフリート実行台帳(`fleet:<name>/`)が起動ごとに約 14 の名前検証警告を撒いていた。`load_all` はマニフェストの無いディレクトリをスキップする — インジェクション経路のみで、成熟度スイープには手を触れない。 |

**エンドツーエンド検証:** 新規 provision → `run`:ステップ失敗ゼロ、全ワーカーが **署名済み** 返信を書き込み(Ed25519 プロヴェナンス)、ループは **Converged** で停止、ルーターは引用付きの Ollama / LM Studio / LocalAI 比較を生成 — 4 ワーカー並行、完全にカーネルサンドボックス内で。

---

*本設計文書は v2.45.0 時点の出荷実装を反映する。「完全な」egress 封じ込め(sbpl pin-to-proxy)と一級の検索 API は、意図的な Phase-3 の後続として残る。*
