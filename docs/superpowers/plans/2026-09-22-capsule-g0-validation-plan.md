# Capsule G0 驗收計畫：先證實保證，再批准核心實作

> Date: 2026-09-22
>
> Status: Ready for G0 validation work；G0 未完成，G1／G2 未批准
>
> Execution skill: mur-executing-plans；單一契約編寫者維持狀態機、schema 與矩陣一致

## 1. 目標、架構與全域限制

**Goal：** 對 [G0 候選契約](../specs/2026-09-22-capsule-g0-contracts.md)逐項產生可重跑的證據與反例，判斷是否有足夠理由申請進入 G1。

**Architecture：** 先用符號模型檢查狀態轉移，再以合成資料與可犧牲儲存實作候選原型，最後在專用硬體驗證 backend 的不可回滾承諾。三層證據分開，不連接 MUR 真實資料、不修改其 production runtime。

**Tech stack：** Capsule 核心與全部 G0 可執行規格使用 Rust。初始抽象模型是零外部依賴的獨立 Rust crate；後續加密原型沿用同一 crate 或拆成獨立 Rust workspace，採經評審的 AEAD／KDF／CBOR／Ed25519 libraries，版本與 lockfile 列入 evidence manifest。TPM／OS backend 只在確認公開 API 與獨立測試 fixture 後實驗。

Global Constraints（沿用已確認規格／本次審核，逐條適用所有任務）：

- 這是獨立於 MUR 的 Agent 記憶專案，MUR 是第一個完整接入者。
- 自動產生的摘要、推論須持續繼承來源的權限與遺忘約束。
- 脫離來源獨立保存、擴大共享範圍須明示授權。
- Managed 不宣稱 L2；Strict 僅在後端通過防回滾銷毀驗收時提供 L2，無法滿足時不得自動降級。
- 批准進入 G0「保證可實現性」驗證，不批准直接進入 G1 核心實作或 G2 MUR 垂直接入。
- 保留不變項應直接進入 conformance suite，不應只留在文件敘述。

## 2. 檔案與責任

現已建立、可直接執行／閱讀：

| 路徑（相對本文件所在 checkout） | 責任 |
|---|---|
| `docs/superpowers/specs/2026-09-22-capsule-agent-memory-system-design.md` | 有條件批准基線及 G0 連結 |
| `docs/superpowers/specs/2026-09-22-capsule-g0-contracts.md` | 狀態、envelope、token、discovery、部署與 MUR sink 契約 |
| `docs/superpowers/plans/2026-09-22-capsule-g0-validation-plan.md` | 測試矩陣、門檻、任務與完成判斷 |
| `docs/superpowers/validation/capsule-g0/Cargo.toml` | 與 MUR workspace 分離的 Rust 驗證 crate |
| `docs/superpowers/validation/capsule-g0/src/lib.rs` | 不含真實秘密的可執行抽象子集 |
| `docs/superpowers/validation/capsule-g0/src/tests.rs` | crash／重放／來源依賴等 Rust tests 與負向對照 |
| `docs/superpowers/validation/capsule-g0/src/main.rs` | 執行 Rust smoke、產生機器可讀 evidence；all 不完整就非零退出 |

後續任務才建立，不代表目前已有實作：

- `docs/superpowers/validation/capsule-g0/src/extended_model.rs`：有界排程探索、grant/revoke 與更完整故障域。
- `docs/superpowers/validation/capsule-g0/vectors/`：固定 envelope/token/materialize/discovery vectors 與已簽核版本。
- `docs/superpowers/validation/capsule-g0/src/crypto/`：Rust 加密原型模組；依賴與 feature 必須鎖定，仍禁止依賴 `mur-*` crates。
- `docs/superpowers/validation/capsule-g0/reviews/`：密碼學與 metadata 評審紀錄，含 reviewer 身分、版本 hash、日期、結論及未解項目。
- `docs/superpowers/validation/capsule-g0/evidence/`：只存合成資料報告、環境 manifest、失敗 seed／最小反例與測試摘要；真實秘密不得進入 git。

## 3. 環境與證據等級

| Env ID | 定義 | 可證實／不可證實 |
|---|---|---|
| A0 | stable Rust toolchain；獨立 crate；固定測試資料 | 抽象條件式性質；不證實硬體、加密或 fsync |
| S0 | 專用 VM，Linux x86_64、8 vCPU、32 GiB RAM、本機 ext4 測試卷；映像 digest 與 kernel 固定 | syscall／WAL／kill／檔案回復；虛擬 TPM 只能作軟體反例 |
| H0 | 可獨占且可犧牲的實機、TPM 2.0 或其他候選後端；公開 API 可操作獨立 test vault | 該具體平台的 power-cut／reset／unseal 證據；不能外推全部同類平台 |
| P0 | 同一台專用 x86_64 主機，8 logical CPUs、32 GiB RAM、本機 NVMe、固定 OS／檔案系統與 governor | 重跑 cold/warm 效能；不把目前未知硬體的 Mac 結果拿來判門檻 |

H0/P0 執行前 MUST 記錄 CPU 型號、RAM、儲存型號與 filesystem/mount options、OS/kernel build、TPM vendor/firmware/driver、runtime/library lockfile hashes、電源策略、實際使用的冷快取方法。缺任何影響測試的欄位標 `environment_incomplete`，不給 pass；未取得設備是 `not_run`，不是以虛擬裝置替代。

本輪能確認的環境只有 Darwin 25.6.0 arm64、macOS 26.6.2、rustc 1.98.1、cargo 1.98.1；硬體型號／記憶體 sysctl 查詢被 sandbox 拒絕，未找到 `tpm2_getcap`。本輪只執行 A0 smoke，不測 real key store，不清除硬體。

## 4. G0 可執行驗收矩陣

下面的數量是最低驗收工作量，允許增加，不可因時間不足把未跑 case 當通過。所有 property failures、canary 洩漏與失敗 seeds 必須保留可重跑反例。

| ID | 方法／輸入 | 環境 | 明確通過門檻 | 本輪狀態 |
|---|---|---|---|---|
| G0-SM | 完整 I01–I10 模型；6 個 mutation、read、crash/recover；最多 4 capsules、3 併發請求、每 history 12 transitions | A0 | 探索全部符合 bounds 的合法 interleavings；每個可達 transition/crash edge 100% 覆蓋；0 invariant violation；負向模型必須產生預期反例 | 初始 smoke pass；完整探索未執行 |
| G0-CRASH | C00–C14（含 C05a/C05b，共 16 cuts）×6 mutations×2 profiles | S0 | 192 個 case identities 全列出；每適用 case 100 seeds；0 不可解釋終態／提前可見／重複副作用；不適用必須有 reviewer 核准理由 | 未執行 |
| G0-LINEAR | 並行 capture/derive/materialize/erase/grant/revoke/read，保存 invoke/return/anchor 歷史 | S0 | 每組衝突對 10,000 histories，1–3 clients；checker 為每個成功 history 找到遵守 real-time order 的合法序列，0 找不到；故意取消再驗證的版本必須被抓到 | 只檢查兩種 derive/erase 抽象提交順序 |
| G0-HW | 真實 backend 的 C05a/b、C06、C10–C14；舊 snapshot/WAL、舊 envelope、直接低層 unseal | H0 | 每適用 cut 每 mutation 至少 10 次；所有允許恢復 case 30 秒內同 root；所有禁止復活 case 0 新解密；reset 後不得自動接管舊 vault；每次實體斷電有外部控制器時間證據 | 未取得 fixture／未執行 |
| G0-CRYPTO | §4 格式評審、官方 primitive KAT、正反 envelope vectors、至少 100,000 parser mutations | S0 | reviewer 無未解阻塞項；KAT 全過；來源移除/重排/移植/nonce/tag/body/profile 變動全拒絕；閉包邊界全過；0 panic/部分明文輸出 | 僅定義候選，未評審／未執行 |
| G0-MEM | 強制 mlock 失敗、kill/crash、記憶體壓力、dump/swap/hibernate 範圍核查 | S0/H0 | 保護 API 失敗必須拒絕；指定不應含明文的 artifacts 中 canary=0；支援／排除的 OS 媒體逐項列明 | 未執行 |
| G0-EXPOSURE | E01–E18、簽章 KAT、偽 credential、舊 key/instance/sequence、工具與多輪 lineage | S0 | 每個固定 vector 通過期望結果；每拒絕類 1,000 變異；0 舊 token 跨 session 成功／0 partial 升 full | 未執行 |
| G0-MATERIALIZE | M01–M14，含前後 crash 與併發兩次兌換 | S0 | 每固定 case 至少100 repeats；0 重複獨立副本；0 同 bytes 新 View 使用舊批准；來源／撤權先線性化必敗 | 僅抽象批准／來源競爭子集通過 |
| G0-DISCOVERY | 3 principals×3 scopes，對不存在/禁止/已刪 ID 查詢，cursor 跨域／過期／撤權 | S0 | 未授權 JSON 欄位、錯誤碼與固定錯誤 body bytes 完全一致；0 hidden_count/epoch；scope/cursor vectors全過 | 未執行 |
| G0-METADATA | 同一負載下目錄、header、I/O 與 API 的可觀察資料 inventory | S0/H0 | 每個欄位有 observer/purpose/retention；無矩陣外未聲明的持久 metadata；reviewer 簽核接受的大小／時間／圖結構洩漏 | 矩陣已定義，未評審 |
| G0-TIMING | missing/forbidden/destroyed 各10,000 次，隨機交錯、同 payload 長度 | S0 | 以固定 classifier 與留出集辨別三類時 accuracy≤40%（chance=33.3%）；超過視為發現洩漏，不以此測試宣稱 constant-time | 未執行 |
| G0-ISOLATION | §8 全部署行，credential 直讀、別人 socket、ptrace、scope 自報、sandbox escape 路徑 | S0/H0 | 明確不互信請求在無隔離部署100%回 UnsupportedIsolation；受支援部署列出的攻擊0成功；能力顯示與實際一致 | 未執行 |
| G0-CANARY | §5 的全部合成 sinks、decode handlers 與負向 leaky fixture | S0 | inventory 中每sink/encoding/crash cut 均執行；禁止儲存中的已知 canary/片段0命中；故意外洩 fixture 100%被偵測 | 未執行；不宣稱 MUR 已封閉 |
| G0-CONFORMANCE | I01–I10＋三態 declaration＋禁止自動materialize＋無反向永久index＋遷移unknown fixture | A0/S0 | requirement-to-vector map 100%；所有已實作 profile 的適用向量全過；unsupported 明確拒絕 | 未執行 |
| G0-PERF | §6 workloads，含 source fanout、冷暖 discover/unlock、整體 epoch rewrap | P0/H0 | 各 workload 的樣本、成本與前後 root完整；符合 §6 門檻；未達不得以跳過權限／依賴換速度 | 未執行 |

## 5. Canary 掃描的具體範圍

測試 corpus 每輪產生唯一 ASCII canary（至少 128-bit entropy 的文字編碼），並配繁中＋中英混合標記。每筆來源綁定 run_id、capsule_id 與允許的存活範圍；只用合成值。

Sink roots 固定在原型 sandbox：`vault/、authority/、tmp/、index/、history/、telemetry/、channel/、proposal/、crash/、exporter/、backup/`。掃描目前與曾寫入的測試 images，包括 staged 檔、WAL、journal、已刪檔的可見 block image、記錄的網路 sink；不得掃使用者真實 home 代替 fixture。

Encoding handlers 必須列明並測：原始 UTF-8、JSON escape、hex、base64、gzip/zstd（若原型有用）、SQLite table/WAL；每個 raw canary 另掃連續 16-byte ASCII 窗口及 8 個漢字窗口。使用未註冊 codec、任意 encoded payload 或原型無法解析的 sink 就標 coverage incomplete。向量/壓縮輸出無法靠字串搜尋證明無洩漏，需 writer instrumentation 與受管儲存結構證據。

允許保留的舊 process View RAM 不算磁碟洩漏；秘密 key buffers、未受管文件或 exporter bytes 不在此例外。正向／負向控制：每一 handler 先放入可知的外洩 artifact，必須找到；再跑 compliant fixture。0 命中但負向控制也找不到，scanner 判失敗。

## 6. 效能負載與 provisional 門檻

固定 corpus seed=20260922；每 capsule 2 KiB UTF-8（半數繁中／混合）；1k、10k、100k 三組；scope 有50%允許／50%禁止。source fanout=1/8/32、depth=1/8/64 分開量測，不能只測無依賴 capsule。

Warm：同 process 完成20次不計樣本暖身，之後1000次；Cold-process：新 process、清空應用快取，100次；Cold-storage 另在專用 fixture 清除 OS cache 或重啟，30次。不得把 Cold-process 報成 Cold-storage。Client concurrency=1/10/100 分開報。

以下是 G0 用來接受／拒絕候選基線的 provisional 門檻，需與實際 P0 manifest 一起凍結；若要更改，保留原門檻與實測理由，不改報表隱藏失敗：

| 負載 | 門檻 |
|---|---|
| 1k capsules、1 client、fanout≤8、Warm recall/discover | p95≤1秒 |
| 10k capsules、1 client、Cold-process | p95≤10秒；超預算有界回 partial 或明確錯誤，不宣稱完整 |
| 100k capsules、Cold-process discovery | 60秒內終止並帶 coverage；不得無限掃描或配置超過8 GiB RSS |
| 1k slots、Strict epoch rewrap＋erase | p95≤30秒；成功前確實撤除舊 wrappers，不能先回成功 |
| 32 sources 批次 unlock | 不可產生部分外部明文；總測試 deadline30秒，逾時分類且清除 buffers |
| 10/100 clients | 每請求有界 outcome、0飢餓逾期後仍無回應；完整報吞吐/p50/p95/拒絕率，不設定未量測的產品容量宣稱 |

若 epoch rewrap 的 O(N) 成本不適合本機產品，G0 應產出失敗結論與替代後端提案；不能把這個效能問題推到 G1 再偷偷改金鑰保證。

## 7. 執行任務與介面

### Task 0：固定基線、證據格式與範圍

Interfaces — Consumes：架構基線、B1–B8、I01–I10。Produces：`run_id、environment_manifest、source_hashes、case_inventory、gate_status`。

- [x] 將架構狀態更新為有條件通過、G0-only。
- [x] 建立候選契約、驗收矩陣與可執行 abstract smoke。
- [ ] 在 S0/H0/P0 provision 後固定環境；未具備設備保持 not_run。
- [ ] 記錄 crypto／metadata reviewer 與版本範圍，不由實作者自行勾成已審查。

### Task 1：擴充可執行狀態機與完整 crash matrix

Interfaces — Consumes：`Anchor、Manifest、PreparedOperation、I01–I10`（契約 §2/3）。Produces：`transition_inventory、crash_inventory、history_traces、counterexamples`。

- [x] 初始 red→green 覆蓋 capture/erase crash cuts、snapshot回復、derive鏈、materialize競爭、lost reply及重啟取消。
- [ ] 新增 grant/revoke/read 與 approve消耗狀態，先寫會抓住反例的 oracle。
- [ ] 實作有界排程探索；bounds／visited states／edges／pruned cases 一併報告。
- [ ] 將每個 Cxx 映射到 6 mutation／2 profile，跑 G0-SM／G0-CRASH／G0-LINEAR；負向模型必須失敗。
- [ ] 序列化最小反例；審核狀態轉移與已執行的覆蓋，才關閉 B1 的軟體部分。

### Task 2：密碼學評審與 envelope 原型

Interfaces — Consumes：`Header0、Capsule、slot_guard、payload_DEK、ordered_sources`。Produces：`reviewed_format_revision、KAT_vectors、negative_vectors、closure_limits、memory_report`。

- [ ] Reviewer 對候選 §4 提出判斷；有阻塞項先回修候選。
- [ ] 建立獨立 Rust experiment；先以官方 KAT 與固定破壞向量做 failing tests，再實作包裝／解鎖。
- [ ] 跑閉包邊界、循環、所有認證欄位移植與 parser mutations。
- [ ] 跑 mlock/dump 失敗與多來源失敗原子性；列出 swap/hibernate 未測範圍。
- [ ] Reviewer 對實際 source hash 與結果簽核；「概念已討論」不計通過。

### Task 3：Exposure 與 materialize 契約原型

Interfaces — Consumes：`ExposureToken、ViewSnapshot、ViewRecord、Approval`（契約 §5/6）。Produces：`E01–E18、M01–M14 results、adapter_conformance_revision`。

- [ ] 固定 deterministic bytes 與簽章測試向量，新增每一拒絕類的先行失敗案例。
- [ ] 實作 session registry、sequence/seal、broker-owned View 與批准 consumption；不整合 MUR。
- [ ] 驗證 broker/host restart 的不同所有權結果與同bytes不同View。
- [ ] 驗證全部來源再驗證與並行批准兌現；失敗不得產生半份獨立副本。

### Task 4：Discovery、metadata、部署與副本契約

Interfaces — Consumes：`scope_handle、opaque_handle、snapshot_token、MemoryProposalV2`。Produces：`leakage_inventory、deployment_matrix_results、sink_coverage、conformance_map`。

- [ ] 建立三 principal／三 scope fixture 與未知/禁止/已刪的等價錯誤案例。
- [ ] 測 cursor/水位綁定與撤權；將 timing觀察獨立報告。
- [ ] 用不同 OS 隔離環境驗證 UnsupportedIsolation；沒有 fixture 不能假設成立。
- [ ] 對所有 sinks 跑 canary handlers／負向控制；MUR v2 reviewer 只驗 schema fixture，不修改 production。
- [ ] Review metadata矩陣；對已接受的物理檔案大小/時間/依賴圖洩漏留下裁決。

### Task 5：實機 backend 與原子提交

Interfaces — Consumes：已審查 epoch／wrap／anchor 契約與 G0-SM 結果。Produces：`backend_capability_report、physical_crash_results、unseal_replay_results`。

- [ ] 驗證 H0 為專用可犧牲設備及空 test vault，記錄操作範圍；不使用現有 MUR／系統金鑰。
- [ ] 先測低層公開 API 是否足以拒絕舊 epoch/同epoch root替換，做不到就回 UnsupportedGuarantee。
- [ ] 實作並故障注入 prepare／fsync／advance／publish；每個適用 Cxx 產生外部時間證據。
- [ ] 測舊 snapshots/WAL/envelopes、斷電與clear/NV重建；一個復活即 Strict fail。
- [ ] 獨立檢閱物理證據，不能從 abstract_smoke 推出 backend通過。

### Task 6：效能與 G0 裁決包

Interfaces — Consumes：全部 gate reports、來源 hashes、已核准環境。Produces：`g0_verdict、open_blockers、proposed_next_stage`。

- [ ] 跑 P0/H0 的固定工作負載，保留每個樣本與單位，回報 provisional門檻結果。
- [ ] 產生 B1–B8 → contract → vector → evidence 的完整追蹤表。
- [ ] 所有安全反例為0、crypto/metadata評審完成、實機與環境證據完整才可提議 G0 pass。
- [ ] 另向使用者提出原規格回修與 G1 授權；report不得自動切換階段。

## 8. 現在可執行的命令與退出碼

從包含本文件的 checkout/work 目錄執行：

```console
cargo run --manifest-path docs/superpowers/validation/capsule-g0/Cargo.toml --bin capsule-g0 -- --suite abstract-smoke --report /private/tmp/capsule-g0-smoke.json
cargo run --manifest-path docs/superpowers/validation/capsule-g0/Cargo.toml --bin capsule-g0 -- --suite all --report /private/tmp/capsule-g0-all.json
```

`abstract-smoke`：通過 exit0，失敗 exit1；report明示只涵蓋符號子集。`all`：任何安全失敗 exit1；未完成／未取得設備／未審查 exit2；只有全部必需 gate 真正通過才可 exit0。**目前預期 all=2，不能算 G0 通過。**

後續實作者把實際 suites 接入同一 evidence 介面；不得手動將 not_run 改為 pass。每個 suite result需包含 case inventory hash、已跑／失敗／未跑數、環境、命令、source hashes 與反例位置。

## 9. 完成的定義

文件已補齊≠阻塞項已解決；抽象 smoke通過≠G0-SM全探索通過；軟體原型通過≠實機 Strict通過；G0通過≠已批准G1。

本輪交付是可執行驗收計畫、候選契約及初始抽象證據。G0 的剩餘工作依 §7 未勾選項執行；不得直接開始 MUR 接入或核心產品實作。
