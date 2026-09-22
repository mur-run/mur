# Capsule G0：可實現性契約與審核回應

> Date: 2026-09-22
>
> Status: G0 candidate contracts；用於反證、原型與評審，不是已通過的安全證明
>
> Authorization: 架構基線有條件通過，僅批准 G0；G1／G2 未批准

本文件補充[架構基線](2026-09-22-capsule-agent-memory-system-design.md)，與 [G0 驗收計畫](../plans/2026-09-22-capsule-g0-validation-plan.md)一起閱讀。只有測試產生證據才更新驗收狀態；新增 schema、狀態機或門檻不代表阻塞項已關閉。

## 1. 審核項目追蹤

| Review ID | 缺口核對結果 | 本文件契約 | 驗收 ID | 目前結論 |
|---|---|---|---|---|
| B1 | 原 §6.3 沒有完整狀態轉移 | §2、§3、§2.4 | G0-SM、G0-CRASH、G0-HW | 交互表與 InDoubt 嵌套規則已補（§2.4，草案）；轉移覆蓋與硬體未驗證 |
| B2 | 原 §6.1 未指定 envelope／閉包／記憶體 | §4 | G0-CRYPTO、G0-MEM | 候選格式待密碼學評審 |
| B3 | 原 token 未定義簽發與失效 | §5 | G0-EXPOSURE | 契約與向量已指定，尚未執行 |
| B4 | 原發現步驟隱含目錄與水位洩漏 | §7 | G0-DISCOVERY | 明示可見性與接受的洩漏 |
| B5 | 原批准未定義可兌現 View | §6 | G0-MATERIALIZE | 以 broker-owned View 解決 process 所有權歧義 |
| B6 | 原硬體與 Authority 交易未定義 | §3 | G0-CRASH、G0-HW | 非回滾 anchor 是待證實後端要求 |
| B7 | 原同 UID 模式未定拒絕條件 | §8 | G0-ISOLATION | 不互信而無隔離時直接拒絕 |
| B8 | 原驗收主要為敘述 | §10、驗收計畫 | 全部 | 可量化門檻已定義，非通過宣告 |

R1、R5、R6 及 R7–R10 的「方向合理」不等於全部機制批准；R4 必須另有密碼學評審。R2／R3 的已確認產品邊界維持。不得以本輪文件更新直接回修原第一層凍結規格或進入 G1。

## 2. L2、Managed erase 與正式狀態

### 2.1 分離四個狀態空間

```text
SlotState       = Absent | Live | DestroyedStrict | ErasedManaged
EffectiveRead   = Readable | DependencyUnavailable | Denied | BackendUnavailable
OperationState  = Accepted | Prepared | DurablePrepared | Anchored | Published
                | Replied | Aborted | InDoubt
VaultHealth     = Ready | Recovering | Quarantined | Unsupported
```

- `Absent → Live → DestroyedStrict` 只發生於 Strict；Managed 的終態為 `ErasedManaged`。終態不可回 Live，key identity 不可重用。
- 衍生 capsule 自身 slot 仍 Live，但任一必要祖先失效時，EffectiveRead 為 DependencyUnavailable；不得因未看到自身 tombstone 就解密。
- Prepared 與 DurablePrepared 尚未出生／死亡，不對正常讀端可見。Anchored 才是相應操作的線性化點。
- `InDoubt` 是呼叫結果尚無法確認，不是允許推測成功的永久 key 狀態。恢復可回到舊提交、確認新提交，或 Quarantined。
- Strict 不支援回 `UnsupportedGuarantee`。沒有 Strict → Managed 的恢復轉移。
- 所有拒絕先遵守 discover 授權；外部未授權 caller 看不到上述詳細 key 狀態。

### 2.2 抽象全域狀態

```text
H = (vault_birth, epoch, manifest_root)       // Strict 非回滾 anchor
D = {immutable encrypted manifests, capsule blobs, cached active_pointer}
M = {sessions, views, token registrations, prepared operations, key buffers}
Manifest = {epoch, parent_root, slots, policy_revision, births, terminals,
            consumed_approval_ids, committed_operation_receipts}
```

`vault_birth` 為新 vault 的隨機 256-bit 身分，不是可重設的 epoch 0。Strict 的 H 不可由一般 D 備份恢復；Managed 用一般交易保護的磁碟 anchor，允許舊磁碟快照恢復這項限制必須明示。

讀取前需 `H.manifest_root` 的完整 manifest、內容 commitment 相符，以及所有必要 capsule blob 存在。active_pointer 只是快取，不能覆蓋 H。遇缺件／不符就 Quarantined，不從歷史 manifest 找一個「還能讀」的版本。

### 2.3 安全不變量與有界活性

| ID | 不變量 |
|---|---|
| I01 | 無有效 grant 或狀態不可確認，不發布任何新的明文結果。 |
| I02 | 每個成功操作只有一個 commit receipt 與一個可解釋線性化點。 |
| I03 | 未被 anchor 選中的 prepared 物件不可出生，不可兌換 key。 |
| I04 | Strict 銷毀後，舊 D、WAL、envelope 或舊 active_pointer 不得恢復解密能力。 |
| I05 | 任一必要來源失效，所有依賴它的衍生品不得新解密／新持久化。 |
| I06 | 來源先失效的 materialize 失敗；已先提交的獨立副本不被回溯刪除。 |
| I07 | 同 operation ID 同輸入回原 receipt／目前終態；異輸入拒絕，不重複副作用。 |
| I08 | broker 重啟不恢復未提交 View、session key 或未兌現批准。 |
| I09 | schema/profile/identity 的變動不得靜默降低任何保證。 |
| I10 | 已提交物件完整且後端可用時，恢復收斂到相同 root；後端不可用有界回錯，不永久卡住。 |

G0 抽象操作的恢復最多走 3 個邏輯步驟：讀 anchor、驗證對應 manifest、修復 pointer／回終態。真實 I/O 不以步數冒充時間界線；原型每次後端呼叫 deadline 5 秒、恢復嘗試整體 30 秒，逾時回 BackendUnavailable。數值是 G0 測試參數，不是正式產品 SLO。

### 2.4 四狀態空間的交互合法性（B1，草案）

§2.1 平行列出四個狀態空間但未定義組合合法性。完整笛卡兒積為 4×4×8×4 = 512 格，逐格列舉不可審查；下列改以「成對合法性 ＋ 一條合成規則」表達，成對表為必要條件，合成規則為 `Readable` 的充分條件。四個空間的作用域不同：`VaultHealth` 為全域，`SlotState` 為每 slot，`EffectiveRead` 為每次讀取嘗試，`OperationState` 為每個 operation ID。

**T1：VaultHealth × EffectiveRead**（已通過 discover 授權的讀取）

| VaultHealth | Readable | DependencyUnavailable | Denied | BackendUnavailable |
|---|---|---|---|---|
| Ready | 合法 | 合法 | 合法 | 合法 |
| Recovering | **禁止**（I01：狀態不可確認不得發布新明文） | **禁止**（判定祖先失效需已確認的 manifest） | 合法 | 合法（Recovering 期間的唯一非授權性答案） |
| Quarantined | **禁止** | **禁止** | 合法（見 B1-Q1） | **禁止**（不得暗示「稍後會好」） |
| Unsupported | 合法（Managed 讀取不受影響） | 合法 | 合法（要求 Strict 保證者） | 合法 |

**T2：SlotState × EffectiveRead**（前提 VaultHealth = Ready 且授權通過）

| SlotState | Readable | DependencyUnavailable | Denied | BackendUnavailable |
|---|---|---|---|---|
| Absent | **禁止** | **禁止** | 合法 | **禁止** |
| Live | 合法 | 合法（§2.1：任一必要祖先失效） | 合法 | 合法（guard unwrap 不可達） |
| DestroyedStrict | **禁止**（I04） | **禁止**（自身終態優先於祖先狀態，不得以祖先原因掩蓋） | 合法 | **禁止** |
| ErasedManaged | **禁止** | **禁止** | 合法 | **禁止** |

**T3：VaultHealth × OperationState**

| VaultHealth | 可新建 Accepted／Prepared | DurablePrepared／Anchored | Published／Replied | Aborted | InDoubt |
|---|---|---|---|---|---|
| Ready | 合法 | 合法 | 合法 | 合法 | 合法（暫態） |
| Recovering | **禁止** | 僅既有者續存 | 僅進入 Recovering 前已 Anchored 者 | 合法 | 合法（InDoubt 的正常居所） |
| Quarantined | **禁止** | 僅為歷史記錄 | 僅為歷史記錄 | 合法 | 合法但**凍結**，見 N5 |
| Unsupported | 僅 Managed | 僅 Managed | 僅 Managed | 合法 | 合法 |

**合成規則 R-READ。** `EffectiveRead = Readable` 當且僅當同時滿足：`VaultHealth = Ready` ∧ 該 slot `SlotState = Live` ∧ 所有必要祖先 slot 皆 `Live` ∧ 授權 grant 有效 ∧ `H.manifest_root` 的 manifest 完整且 commitment 相符（§2.2）。五項缺一即非 `Readable`，且回報順序固定為：授權（§2.1 末項）→ VaultHealth → 自身 SlotState → 祖先 → 後端可用性。固定順序是為了讓錯誤碼不洩漏更高敏感度的資訊。

**B1-Q1（待裁決）。** `EffectiveRead` 沒有表達「vault 已 Quarantined」的值，T1 只能回 `Denied`，這使「你沒有權限」與「此 vault 已不安全」共用同一個外部可見結果，與 §2.1 末項的授權分層意圖相衝。兩條出路：於 `EffectiveRead` 增設 `VaultQuarantined`，或明文記載此合併為蓄意設計並說明其不洩漏理由。此項不阻塞 B2。

#### 2.4.1 InDoubt 的嵌套恢復規則

R2 指出「判定 InDoubt 所需的 `read_anchor` 自身 InDoubt」的嵌套情形未處理。**型別層已排除無限嵌套**：§3.1 的簽章為 `read_anchor() -> Anchor | BackendUnavailable`，不含 `InDoubt`；只有 `advance_epoch` 會回 `InDoubt`。恢復程序讀的是不會再 InDoubt 的那一支，因此這是一個有界迴圈，不是遞迴。餘下需要明文的是它的邊界與終態：

| ID | 規則 |
|---|---|
| N1 | 恢復只呼叫 `read_anchor`；其回傳域不含 `InDoubt`，故不產生第二層待決。未確認一律顯現為 `BackendUnavailable`。 |
| N2 | 單次呼叫 deadline 5 秒、整體恢復 30 秒（§2.3）。逾時後 `VaultHealth = Recovering`、`OperationState` 維持 `InDoubt`，回 `BackendUnavailable`。不得因逾時而推定 `Aborted`。 |
| N3 | `InDoubt` 必須與 `operation_id`、`expected_anchor`、候選 `manifest_root` 一同持久化。重啟後若無此三元組即無法區分已提交與未提交，I07 的冪等回應將不可實現。 |
| N4 | 每次恢復嘗試皆為唯讀且冪等，不產生第二次副作用（§3.2）。重試次數在時間上不設上限，但每次嘗試有界；此為迴圈的活性條件，與 I10 的有界回錯一致。 |
| N5 | `Quarantined` **凍結** `InDoubt`，不解決它。未知 root 或 `vault_birth` 不匹配時進入 `Quarantined`，該 operation 既非 `Published` 亦非 `Aborted`。將其回報為 `Aborted` 會在它實際已提交時同時違反 I02 與 I07。對呼叫端的正確回答是「不可確認」，並保留三元組待後端恢復。 |
| N6 | `InDoubt` 不是 key 狀態（§2.1）。其存在不改變任何 `SlotState`；T2 依自身 `SlotState` 判定，不因有未決 operation 而讓 `DestroyedStrict` 回到可讀。 |

上述 N1–N6 與 T1–T3 為 G0-SM 的 oracle 來源：負向模型必須在違反任一格時失敗。B1 的軟體部分需在 `validation/capsule-g0` 補上對應轉移後才可關閉。

## 3. 硬體／Authority 的候選提交與恢復協定

### 3.1 後端必須證實的介面

```text
read_anchor() -> Anchor | BackendUnavailable
prepare_epoch(expected_anchor, operation_id) -> EpochHandle
wrap_guard(epoch_handle, slot_id, guard) -> WrappedGuard
advance_epoch(expected_anchor, epoch_handle, manifest_root)
    -> Advanced(anchor) | Conflict | InDoubt
unwrap_guard(current_anchor, manifest_root, slot_id, wrapped_guard)
    -> Guard | Denied | BackendUnavailable
```

這些是必須由實機證實的語義，**不是宣稱 TPM 或 Secure Enclave 已提供這套 API**。單一 counter 加一份普通磁碟 root 尚不能證明此契約：必須證明同 epoch 的替換、舊 epoch 的直接 unseal、低層 API 繞路及 reset 都不能恢復被撤除的 guard。

候選基線採每個提交產生新 epoch wrapping domain，將所有存活 slot guard 重新包裝；新 manifest 不含已銷毀 slot 的可解密 guard。這會有 O(存活 slots) 成本，刻意先用可審查基線，不預先宣稱高吞吐。後續任何增量金鑰樹最佳化須另證 I01–I10。

wrap 的綁定使用 vault_birth／epoch／operation_id／slot_id，不把包含 wrapped_guard 的 manifest_root 反過來當 wrap 輸入，避免循環定義；root 由最後的 anchor commit 綁定。是否能在硬體條件下同時做到域切換與 commitment 不可替換，是 G0-HW 的核心問題。

### 3.2 固定順序

1. 驗證請求、完整來源閉包、批准／session；建立候選內容與新 epoch handle。昂貴推理已在此之前完成。
2. 持有 vault writer lock，重新核對 expected anchor、政策與來源，產生完整 candidate manifest。
3. 將 encrypted capsule、guard wraps、manifest 全部持久化：檔案 fsync、原子 rename、目錄 fsync。WAL 模式必須讓尚未 checkpoint 的 commit 也符合重啟可讀，不能只測 clean shutdown。
4. 呼叫 `advance_epoch`；它必須原子選中完整新 root 並撤除所有舊 epoch wraps 的解封權。此時 capture／derive／materialize／erase／grant／revoke 生效。
5. 更新 Authority active_pointer 的 tmp／fsync／rename／directory fsync；它不是第二個提交點。
6. 回 receipt。清理未選中的 prepared encrypted blobs；GC 失敗不改變線性化結果。

在 2–4 期間必須排除同 vault 的來源 L2／政策修改。所有讀取在發布結果前依同一 anchor／政策的讀取邊界再驗證；一次多來源解鎖只回全部結果或錯誤，不先 stream 部分明文。

收到 InDoubt 不產生第二次副作用；先 read_anchor。它仍為舊 root 則尚未提交；等於候選 root 則已提交；後端不可讀則維持不可用。未知 root／vault_birth 不匹配直接 Quarantined。

### 3.3 Crash matrix

每行要套用到 `capture_batch、derive、materialize、erase、grant、revoke`，兩種 profile 分開記錄；erase 的成功文字依 profile 區分。

| Cut ID | 注入點 | 重啟後可承認的結果 | receipt／freshness |
|---|---|---|---|
| C00 | 請求接受前／解析中 | 舊 root；無新操作 | 無 receipt |
| C01 | 建立 handle 後、blob 尚未 fsync | 舊 root；清理未選中物件 | 無新水位 |
| C02 | blob fsync 中／目錄 fsync 前 | 舊 root；半檔不可讀 | 無新水位 |
| C03 | manifest／wraps 持久中 | 舊 root；不可用半份 manifest | 無新水位 |
| C04 | 全部 fsync 完成、advance 前 | 舊 root；重啟不自動兌現暫態 View | 無新水位 |
| C05a | advance 呼叫結果遺失，硬體未前進 | 舊 root；未提交 | 重試前重新授權 |
| C05b | advance 呼叫結果遺失，硬體已前進 | 新 root；必須補發布 | 回原 receipt |
| C06 | anchor 新、pointer 舊 | 從 H 指定 manifest 恢復新 root | min_commit 可滿足或回明確不可用 |
| C07 | pointer rename／directory fsync 中 | 新 root | 不倒回舊 root |
| C08 | pointer 已發布、reply 尚未送達 | 新 root | operation ID 回原 receipt |
| C09 | reply 已送達、GC 中 | 新 root；孤兒仍不可被讀取 | 成功結果不可倒退 |
| C10 | H 新、對應 manifest/blob 遺失或損毀 | Quarantined；不能復活但可能失去可用性 | 不回成功讀取 |
| C11 | 恢復任意舊 D／WAL／舊 envelope | Strict：原 H 或 Quarantined；Managed：記錄其可回滾限制 | Managed 不回 L2 receipt |
| C12 | 正常斷電重啟硬體 | anchor 身分及不可回滾性維持 | 依匹配 root 恢復 |
| C13 | 硬體 clear／更換／NV 重建 | 舊 vault 不能因新 epoch 0 重新被接受 | Quarantined／不可恢復；不自動重新 provision |
| C14 | 硬體暫不可達／逾時 | Recovering → BackendUnavailable | 不猜提交結果 |

硬體 clear／NV 重建只在全新、可犧牲的專用 fixture 執行，禁止對目前使用者裝置或 MUR 真實 key store 測試。

### 3.4 Abstract model 的涵蓋與限制

[Rust 抽象模型](../validation/capsule-g0/src/lib.rs)提供 capture／derive／materialize／erase、prepare／flush／advance／publish／restart 及磁碟 image 回復；[測試](../validation/capsule-g0/src/tests.rs)涵蓋相應 crash cuts、兩種 derive/erase 提交順序、間接來源、批准、冪等與一個故意可回滾 anchor 的負向對照。crate 有自己的 `Cargo.toml` 與 `[workspace]`，不加入 MUR workspace。

這個初始模型 **不含**真實加密、硬體、fsync、批准簽章、OS 身分、grant/revoke、完整任意排程探索或記憶體防護。`approved=True` 只代表假設已通過外部批准檢查的符號輸入，絕非產品 API。單元測試通過只記為 `abstract_smoke=pass`，G0-SM 的完整有界探索仍未完成。

## 4. Envelope 候選格式與密碼學審查範圍

### 4.1 固定編碼與秘密用途

候選 wire encoding 採 RFC 8949 deterministic CBOR：固定 schema 的 integer-key maps；拒絕重複 keys、indefinite lengths、非最短編碼、未知必要欄位與 floats。Schema 版本固定為 `capsule-envelope-g0-v1`，不與未來 production v1 混用。

```text
Header0 = {format, suite, vault_birth, profile, key_id, kind,
           capsule_salt, ordered_sources[{capsule_id, key_id}]}
Capsule = {header: Header0, body_nonce, body_ciphertext,
           envelope_layers[{layer_index, nonce, ciphertext}]}
```

- `key_id`、`slot_guard`、`payload_DEK`、`capsule_salt` 各為獨立 CSPRNG 256-bit 值；DEK 只加密這顆 payload，slot_guard 只保護它的最後一層 DEK envelope。
- 候選 primitives：AES-256-GCM（96-bit nonce、128-bit tag）、HKDF-SHA-256、SHA-256。每個新 payload DEK 只加密一次；包裝子金鑰按 vault／child／source／layer／purpose 域分離。重試重用已產生密文，不在相同子金鑰下重新產生另一份內容。
- Body AAD 為 `encode(["capsule/body/g0/v1", Header0])`。先產生包含 tag 的 body_ciphertext，再算 `body_digest = SHA256(encode([body_nonce, body_ciphertext]))`；避免 body AAD 含自身密文 digest 的循環。
- source layer key 為 `HKDF(source_DEK, salt=capsule_salt, info=encode(["capsule/source-wrap/g0/v1", Header0, source_key_id, layer_index]), L=32)`。
- 各層 AAD 為 `encode(["capsule/wrap/g0/v1", Header0, body_digest, layer_index, source_key_id_or_guard])`；最外層 guard key 為 `HKDF(slot_guard, salt=capsule_salt, info=encode(["capsule/guard-wrap/g0/v1", Header0]), L=32)`；guard 的 layer_index 等於來源數量，source_key_id_or_guard 為文字 `"guard"`，與 bytes 型來源 key_id 分離。
- 直接來源按 key_id 的 unsigned bytes 升冪排列、拒絕重複。來源層 layer_index 從 0 起。第一層加密 payload_DEK，後續層加密前一層完整的 `encode([nonce, ciphertext])`，最後包 guard 層；解鎖逆序。wire 的 envelope_layers **只含最外 guard 層**，內層僅在上一層解密後可見，禁止同時暴露可繞過 guard 的平行內層副本。移除／重排／更換來源、nonce、body 或跨 capsule 移植都必須失敗。
- Ciphertext content address 為 `SHA256(encode(Capsule))`，不把它自身放回 Header0。

這是供審核的候選結構；最終 integer field ID 表、二進位測試向量與 parser 尚待 G0-CRYPTO 交付，不是引用標準就自動安全。密碼學 reviewer 必須評估組合、nonce 策略、chosen-ciphertext、domain separation、context binding、快取與備份旁路，簽核報告及 vectors 後才可凍結格式。[CBOR](https://www.rfc-editor.org/rfc/rfc8949)、[HKDF](https://www.rfc-editor.org/rfc/rfc5869)、[GCM](https://csrc.nist.gov/pubs/sp/800/38/d/final)

**凍結範圍界定。** 自 §7.1 的 `ordered_sources` 裁決起，本節已凍結的是 `capsule-envelope-g0-v1` 的**結構與 AAD 綁定拓撲**：Header0 欄位集合、`ordered_sources` 明文性與排序規則、body_digest 的產生順序、各層 AAD 的組成項與 layer_index 語義。密碼學**參數**不在凍結範圍內——primitive 選擇、HKDF info 字串內容、nonce 派生策略、§4.2 的資源上限數值，均為 B2 評審對象，評審後修訂不算破壞凍結。Reviewer 應對參數提出意見，不需在結構與參數間二擇一。

### 4.2 閉包與資源上限

採三色 DFS：進入節點標 gray，離開標 black；遇 gray 拒絕循環；black memo 僅在本次操作內有效。驗證 capsule/key 對應、profile/vault、來源狀態與 scope 後，才加入待解鎖拓撲序列。G0 上限：直接來源 32、最長來源邊數 64、不同節點 4096、解析 envelope 1 MiB；任何上限超出回 `DependencyLimitExceeded`，不得截短來源。

同批次來源只允許已拓撲排序、批次內可驗證的前置物件；沒有有效前置資料就拒絕。以上演算法與上限尚未由初始 smoke model 完整覆蓋，須在 G0-CRYPTO 原型執行 0／1／31／32／33、63／64／65 與 4095／4096／4097 邊界向量。

批次先驗證完整閉包與資源預算，使用本次操作內 memo 減少重複解鎖；任一解鎖／最終再驗證失敗，不向 client 發送部分明文，清除全部新取得的 key buffers。

### 4.3 記憶體、swap 與 core dump

真實原型必須使用可清除、可鎖頁的固定秘密 buffers，禁止把 guard/DEK 複製成一般字串。秘密生命期只限當次解鎖／提交，finally 清除；不將 key cache 跨請求保留。core dump 在啟動時停用，平台支援的排除 dump／鎖頁 API 都要檢查回傳結果。

秘密頁無法鎖定或 dump 防護失敗：密鑰操作回 `UnsupportedMemoryProtection`，不得因 vault 是 Managed 就默默忽略。G0 需在隔離測試主機施加記憶體壓力並檢查 crash artifacts；查不到字串不構成對整個 OS、hibernate 或虛擬機 snapshot 的數學證明，範圍及未覆蓋媒體必須列入 report。

## 5. Exposure Token schema 與 Adapter 信任鏈

### 5.1 身分與註冊

Broker 是 token 簽發者。每次啟動產生新的 broker_incarnation（隨機 256-bit）及只存 RAM 的 Ed25519 signing key。Client 從已驗證本機連線取得該次公開金鑰；不能接受模型提供的 key。

Trusted Adapter 必須有控制平面核准的 package digest／version、conformance 報告與獨立保護的 credential。Broker challenge-response 驗證 credential、OS 連線身分及部署隔離條件後，核發新的 adapter_session_id；host_instance_id 每次 host 啟動重新隨機產生。

同 PID／同 UID／同檔名／自己回報版本不能認證可信 Adapter。這是軟體信任與 OS 保護的條件式保證，不稱為遠端硬體 attestation。

### 5.2 Token

```text
ExposureToken = {
  version: 1, type: "capsule-exposure-g0", issuer_key_id,
  broker_incarnation, vault_birth, principal_id,
  adapter_session_id, host_instance_id, inference_id, branch_id,
  sequence, exposure_set_commitment, coverage: full|partial,
  policy_revision, issued_mono_ms, expires_mono_ms, nonce,
  audience: "capsule-derive", signature
}
```

簽章為 Ed25519，對 `encode(["capsule/exposure/g0/v1", unsigned_fields])` 的 deterministic CBOR 計算；signature 不包含自己。Opaque IDs／nonce 為 256-bit 隨機值。G0 token TTL 為 300 秒；monotonic time 只在對應 broker incarnation 內有意義，不用牆上時間倒退延長有效期。[Ed25519](https://www.rfc-editor.org/rfc/rfc8032)

實際 exposure set 存在 broker 的有界 RAM registry，token 只存 commitment；不能以 token 自帶來源列表取代 registry。每次受管資料交付都先增加 sequence 與來源集合，才交付明文。模型呼叫結束由可信 Adapter 提交 `seal_inference`，包含已確認交付序號及輸出 digest；提交時 token 必須對應 sealed 最新集合與輸出，舊序號拒絕。

### 5.3 失效、重放與 lineage

- Broker 重啟：RAM signing key、registry 與 incarnation 消失，所有舊 token 拒絕。
- Host 重啟：新 instance／session，舊 token 不能在新連線兌換。連線關閉或授權撤回，舊 session 失效；不要把 heartbeat 當絕對的即時死亡偵測。
- `derive` 的一次 output commit 綁 `inference_id + output_digest + operation_id`。相同操作重試走 receipt；不同輸出不能重放舊 sealed token。
- Multi-turn lineage：上一輪 assistant／summary 有受管 refs 時，這些 refs 加入下一輪 exposure；assistant text 不能以 capture 重新標為獨立 Observation。
- Tool result：可信來源工具帶 refs，Broker 核對；工具參數若使用受管內容，Adapter 至少保守繼承該次推理已知 exposure。來源不可完整涵蓋時設 partial，不可由模型升回 full。
- partial Adapter 不能使用聲稱完整來源追蹤的自動 derive／materialize 註冊路徑；可由使用者走明示 external import，但保留 partial 標記與原有副本邊界。

### 5.4 Client 能力矩陣與向量

| Client | 可用 | 禁止 |
|---|---|---|
| 通過 conformance 的可信 host | capture 原始事件、sealed derive、註冊待批准 View、授權 recall | 自行簽批准、改 scope／profile |
| 未整合 MCP client | capabilities、scope 內 recall／explain | 自動回存模型回答、補造 exposure、claim full tracking |
| 使用者控制介面 | grant/revoke、批准 materialize、forget、明示 import | 以批准繞過已失效來源或 profile |

G0-EXPOSURE 固定向量：E01 正常簽發；E02 改 principal；E03 改 audience；E04 改 source commitment；E05 過期；E06 舊 broker；E07 舊 host／session；E08 舊 sequence；E09 未 seal；E10 輸出 digest 不同；E11 同 operation 重試；E12 不同 operation 重放輸出；E13 partial 升 full；E14 模型偽裝原始 observation；E15 工具參數 lineage；E16 多輪來源繼承；E17 未整合 MCP 回存；E18 撤權後舊 token。允許向量僅 E01/E11 與符合契約的 lineage 情境，其餘必須拒絕或維持 partial。

## 6. Materialize 請求、View 與批准

### 6.1 唯一可兌現的 View 所有者

Materialize 採兩階段：可信 Adapter 先提供精確 payload、sealed exposure 與 transform metadata，Broker 驗證後在自己的 RAM 建立 **新的 broker-owned View**。這不是跨 process 共享原 host View 的身分；UI 預覽與批准針對這份 broker View。

Host 在註冊完成前崩潰，沒有可兌現 View。註冊完成後 host 崩潰，不會自動摧毀仍存活的 broker View，但原 host token 不能用在新 session；使用者仍可在 TTL 內批准已由 broker 接收的精確內容。Broker 崩潰則這份 View 與未兌現批准全部失效。這個所有權界定必須在 UI／協定一致，不靠無法證明的「來源 process 此刻一定活著」。

### 6.2 Snapshot 與 digest

```text
ViewSnapshot = {
  schema: "capsule-view-g0-v1", payload_media_type, payload_bytes,
  ordered_supporting_refs, frozen_provenance, transform_metadata
}
ViewRecord = {view_id, broker_incarnation, requester_principal,
              snapshot, snapshot_digest, registered_mono_ms, expires_mono_ms}
snapshot_digest = SHA256(encode(["capsule/materialize-view/g0/v1", ViewSnapshot]))
```

G0 View TTL 為 900 秒。payload_bytes 是精確 bytes；Unicode、空白、來源順序與 transform 變動均形成不同 snapshot。即使重新推導的 bytes 一樣，新 view_id 仍不同，不能兌換舊批准。Digest 僅在受保護控制平面傳遞／儲存，不公開成可猜測內容的永久 hash index。

### 6.3 批准與兌現

```text
Approval = {version, approval_id, controller_principal, requester_principal,
            broker_incarnation, view_id, snapshot_digest,
            source_set_commitment, target_scope, action: "materialize",
            expires_mono_ms, nonce, signature}
materialize = {operation_id, view_id, approval}
```

批准由已驗證控制介面憑證簽署，綁定完整用途；client 不能送 `approved=true`。Broker 從 RAM 取得 bytes，重新計算 digest；不存在／到期就 `ViewExpired`，不從磁碟讀回、不接收 client 替換內容。

兌現前依序驗證批准簽章及角色、view_id／incarnation／digest／sources／scope／期限，查完整來源閉包與現行政策。批准不保留來源 Live 特權，也不授予任意讀取或更大 scope。

同 approval_id 的並行請求先作 RAM reservation，只有同 operation 可重試；消耗批准與新 capsule 的出生寫入同一個 Anchored manifest。anchor 前 crash：無持久提交，但原 broker View 消失，不能以舊批准重做。anchor 後 crash：只補發布／回既有 receipt，不要求 View 復活。

固定向量 M01 正常；M02 bytes／digest 改動；M03 來源集合改動；M04 scope 擴大；M05 過期；M06 broker 重啟；M07 同 bytes 新 View；M08 批准後來源 L2；M09 批准後 revoke；M10 兩 operation 共用批准；M11 同 operation lost reply；M12 anchor 前／後 crash；M13 host 註冊前／後 crash；M14 控制憑證冒用。M11 只回原 receipt；M12/M13 依上述所有權及線性化點判斷，不籠統一律成功。

## 7. Discovery API 與 metadata 洩漏

### 7.1 發現能力來源

不掃描全域語義索引。Authority 維護**加密的授權與出生目錄**，按授權 scope 選取 opaque capsule IDs，再解開允許的 per-capsule index。目錄可持久化但只含控制／發現所需欄位；它是 R5 的顯式新增能力，不能繼續宣稱不存在發現目錄。

目錄版本與政策版本被同一 anchor commitment 保護。主題、名稱、全文、embedding、來源 DAG 細節不放公開目錄。直接來源引用留在公開 envelope header，會洩漏依賴結構。**此取捨已裁決：保留 `ordered_sources` 明文於 Header0，洩漏在物理觀察者那一列無條件接受**，不能寫成「無 metadata 洩漏」。裁決理由有二：其一，`ordered_sources` 每項為 `{capsule_id, key_id}`，只拿掉 `key_id` 而留下 `capsule_id` 並不改變圖結構可見性，該選項付出成本卻不改變本列結論；其二，`ordered_sources` 的明文性是 §4.1 各層 AAD 綁定的前提，移除後反移植、反重排、反來源移除（§4.1 末段要求的失敗性質）將失去綁定物，唯一替代是改用帶 blinding 的 hiding commitment，等同更換 wire format、AAD 與閉包驗證路徑，並新增 commitment 正確性評審項。重新提議移除者須先推翻這兩點。

```text
discover(scope_handle, page_size<=100, cursor?)
  -> {items[{opaque_handle}], next_cursor?, coverage, snapshot_token}
describe(opaque_handle) -> authorized_descriptor | Unavailable
```

scope_handle 由授權 session 提供。cursor 是不透明、MAC/AEAD 保護的 scope／principal／政策版本／目錄快照／offset 綁定值，G0 TTL 60 秒；撤權、snapshot 失效或跨 principal 重用即拒絕。回傳不帶全庫 total_count 或原始 epoch。

snapshot_token 與 min_commit receipt 都對 principal／scope 綁定並加密，不直接給單調序號；內部仍可比較水位。使用者只能觀察自己有權範圍的新資料，不能靠全域提交次數估計其他 Agent 活動。

### 7.2 洩漏矩陣

| 觀察者 | 存在性 | 數量 | 更新頻率／水位 | 主題／依賴 |
|---|---|---|---|---|
| 有 discover/read grant 的 principal | 可枚舉自己範圍 | 可從枚舉累積估算；不宣稱隱藏 | 可觀察自己範圍變化；opaque 水位 | 授權後可看 descriptor／來源 |
| 有其他 scope grant 的 principal | 對目標 scope 不可查 | 不回 total／hidden_count | 不給全域 epoch | 不回未授權 metadata |
| 無 grant／猜 ID 的 client | missing、forbidden、destroyed 統一 Unavailable | 不可從 API 枚舉 | 不回水位 | 不可查 |
| 可讀原始磁碟的 OS／備份觀察者 | 可見檔案／密文物件存在 | 可估物件數／大小 | 可見檔案時間、I/O；接受此洩漏 | Header0 明文帶 `ordered_sources[{capsule_id, key_id}]`，依賴圖結構對此觀察者可見；語義與內容不明文。已裁決接受（§7.1） |
| broker／受信任管理者 | 執行職責可見 | 可見 | 可見 | 屬可信邊界，不能宣稱向它隱藏 |

不承諾 ORAM、流量隱藏或消除共享硬體 timing side channel。未授權 API 的錯誤碼、結構、欄位與固定錯誤 payload 長度必須相同；G0 timing 差異門檻只作洩漏偵測，通過不能升格成 constant-time 證明。

## 8. 部署矩陣與 UnsupportedIsolation

| 部署 | 可用範圍 | 必須拒絕 |
|---|---|---|
| 單一可信 host 內嵌 | 同信任域的自有記憶 | 要求跨不互信 host 的隔離保證 |
| 多個可信 runtime、同 UID、無 sandbox | 明示 trusted_host_only，共享需授權 | 任一要求 mutually_untrusted 時回 UnsupportedIsolation |
| 多 runtime，OS principal 分離或通過 escape/ptrace/credential 防護測試的 sandbox | 驗證範圍內的受控共享 | 任一前提失效即拒絕新跨域操作 |
| 未註冊 Adapter／普通 MCP client | §5.4 的受限操作 | 受限 client 不能冒充 full-tracking host |
| 有 TPM／Secure Enclave、但 OS 隔離不足 | 只按實際 trusted_host_only 能力使用 | 硬體存在不能覆蓋 UnsupportedIsolation |

`capabilities` 與每次 session 建立回 `profile、isolation、provenance_coverage、unsupported_reasons`。UI 在 grant 前顯示接收方信任域與實際隔離等級；請求 `required_isolation=mutually_untrusted` 無法滿足時，不提供「繼續就算同意較弱模式」的隱性路徑。

## 9. MUR 副本封閉契約

G0 僅用合成資料 inventory、sink recorder 與 schema fixtures 驗證候選，**不修改 MUR runtime／reviewer**。實際 MUR 封閉驗收屬 G2，不能因 G0 測了 fixture 就宣稱 MUR 已符合。

| Sink | 已核對的內容路徑 | Capsule 模式目標 | G0 證據／G2 證據 |
|---|---|---|---|
| history JSON | `ConversationStore::persist` | 受管 capsule／reference | static inventory／真實 task canary |
| FTS／LanceDB／rollup | conversations content 欄 | query bridge、受管 index projection | 模擬 sink 與 schema／實際索引檔掃描 |
| telemetry／OTel | `Event::Routing.task_summary` | 只留無語義 metadata 或受管引用 | 序列化 fixture／本機及 exporter 輸出 |
| channel | `append_self_reply` 的 reply_text | 受限 reference，讀取時授權 | message variant fixture／Hub 全路徑 |
| note proposal | `MemoryProposal.manifest` | 下述 capsule_ref variant | parser／serializer fixture；G2 新 reviewer |
| crashlog／stderr | panic payload 與 redirection | 無受管明文；隔離 dump／log | crash sink canary／真實流程 |
| temp／staging／WAL／backup | 模式切換及後端暫存 | 只有受管密文與允許 metadata | sandbox 全檔案掃描／各真實 backend |

候選 proposal variant：

```text
MemoryProposalV2 = {
  version: 2, kind: "capsule_ref", proposal_id, agent_principal,
  vault_handle, capsule_handle, requested_action: "review_materialize",
  target_scope_handle, expires_at, signature
}
```

不含全文、摘要、語義 note name、excerpt 或原始 query。簽章涵蓋所有 unsigned fields 與獨立 domain。既有 v1 manifest 模式僅用於 legacy 或已批准獨立匯出；舊 reviewer 遇到 v2 拒絕，不解析出空 manifest 當成功。

審核顯示內容需當下授權。拒絕／到期時不曾寫出全文；接受時走 §6 的具體批准與 materialize，再由 G2 Adapter 建立獨立 note。reference 存在本身可能透露一次提案活動，inbox 必須在接收者授權範圍內，納入 metadata report。

## 10. 通過不能只靠模型

所有保留不變項映射到 G0 conformance：不可變 capsule／完整提交（I02/I03）、來源追蹤（I05/I08）、三態 declaration（E13/E14）、禁止自動 materialize（M14）、L2 無 undo／escrow（I04）、無未授權發現（§7）、無永久反向 materialization lookup（sink inventory）、遷移 unknown 不猜成功（protocol fixture）。遷移本身不在 G0 實作範圍，fixture 不能當作遷移實作通過。

G0 最終 report 必須同時列出 `abstract、software_prototype、cryptographic_review、physical_backend、adapter_contract` 的結果與未測範圍。失敗、未執行、未具備設備不能被合併為通過。G0-SM／HW／CRYPTO 未完成之前，R4–R10 機制及 G1 仍未批准。
