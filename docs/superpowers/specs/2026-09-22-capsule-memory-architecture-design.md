# Capsule 記憶第一層架構設計

> Date: 2026-09-22  
> Status: Draft for review（第一層裁決已凍結；尚未進入實作）  
> Scope: 儲存、金鑰生命週期、遺忘、遷移、provenance、生命週期事件

## 0. 文件地位

本文件整理已完成的第一層架構裁決，作為後續實作與第二層設計的硬邊界。它不是索引樹、多行程執行或 reducer 的實作計畫。

規範詞義：

- **MUST / MUST NOT**：凍結不變量；若不可行，必須顯式回修本文件，不得由實作者局部繞過。
- **SHOULD / SHOULD NOT**：目前偏好，可透過評審調整。
- **OPEN**：刻意留給後續設計，不得假裝已有答案。

每項凍結裁決都必須能回答四件事：觸發條件、行為、崩潰語義、錯誤代價。

---

## 1. 範圍與非範圍

### 1.1 範圍

1. immutable capsule 的持久化格式與提交邊界。
2. 每 capsule DEK 與 Key Authority（KA）的生命週期。
3. L2 遺忘及其可觀察保證。
4. capsule 遷移及終止語義。
5. 衍生內容、凍結 provenance、暫態 view 與 materialize。
6. 出生、死亡、未確認及有界審計事件。
7. 單寫者及跨 process 可見性的第一層約束。

### 1.2 非範圍

以下另案設計，不能偷偷寫入本層：

- 名詞／動詞／時間索引樹的實際結構。
- 多行程排程、worker 拓撲與 IPC 模型。
- reducer 的輸入選擇、觸發時機與避免 provenance 洗白的演算法。
- LanceDB／Tantivy 的整合方式。
- 跨 process 共享 view。
- 搜尋排名、查詢規劃與 UI 視覺設計。

---

## 2. 核心名詞

| 名詞 | 定義 |
|---|---|
| Capsule | content-addressed、不可變的持久化單元；包含 events blob 與加密的 per-capsule index。 |
| DEK | 單一 capsule 專屬的資料加密金鑰。 |
| `key_id` | 隨機 256-bit、由 CSPRNG 產生且永不重用的 DEK 身分。 |
| KA | Key Authority；唯一永久可變、受 TPM 保護的金鑰權威。 |
| L2 | 銷毀指定 DEK 的不可逆儲存層遺忘操作。 |
| View | 讀取／推理產生、只存在於產生它之 process 記憶體的暫態物件。 |
| `H(V)` | view 的歷史／內容識別，用於 provenance 聲明；不保證可由持久儲存解析。 |
| Materialize | 將仍可合法持久化的 view 明文，以新 DEK 重加密成獨立 capsule 的明示操作。 |
| Frozen provenance | 建立衍生品時固定、之後不可修改的來源聲明。 |
| Nested AEAD | 讓持久衍生品的可解密性依賴全部直接來源 DEK 的加密結構。 |
| Tombstone | KA 中永久且最小的 `key_id → Destroyed` 記錄。 |

「遺忘」不是本體論上的全世界失憶，而是從某一時刻起，儲存層不再能解密或產生新的持久副本。

---

## 3. 資料模型

以下為邏輯結構，不預先綁定序列化格式：

```text
CapsuleHeaderPlaintext {
  format_version
  content_address
  encrypted_payload_locator
  encrypted_index_locator
  dependency_key_ids[]   // 僅供 GC／可解密性判斷所需
}

CapsuleEncryptedBody {
  events_blob
  per_capsule_index
  provenance
  lifecycle_metadata
}

KAEntry =
  | Live {
      key_id,
      wrapped_dek,
      wrapping_domain,
      created_at
    }
  | Destroyed {
      key_id
    }

FrozenProvenance {
  declaration_mode
  direct_sources[]
  has_upstream_provenance
  transform_metadata
}

DeclarationMode =
  | mandatory_tracked
  | optional_human
  | materialized_view

MaterializedViewDeclaration {
  view_hash: H(V)
  direct_supporting_sources[]
  transform_metadata
}

MigrationFinalization =
  | Finalized(success)
  | Finalized(failed)
  | Finalized(unknown)
```

### 3.1 明文 header 最小化

明文 header MUST 只保留 GC、格式辨識與定位所必需的資料。主題、摘要、名稱、全文、明文索引及 provenance 細節 MUST NOT 放入 header。

`dependency_key_ids` 是加密依賴描述，不是可搜尋的語義標籤。

---

## 4. 凍結不變量

### 4.1 Capsule 建立與提交

**觸發條件：** 寫入 session 產生待持久化事件。

**行為：**

1. 一個寫入 session MUST 對應一顆 capsule。
2. 每顆 capsule MUST 使用獨立 DEK 與全新 `key_id`。
3. events blob 與 per-capsule index MUST 在同一 capsule 邊界形成。
4. capsule 一旦提交即不可變；修改代表建立新 capsule。
5. 持久內容 MUST 以內容定址。
6. 同一 capsule 的持久化提交 MUST 只有一個寫者。

**崩潰語義：** 提交必須是「完整 capsule 可見」或「未提交 capsule 不可見」；不得留下被正常讀取路徑視為有效的半成品。孤兒 blob 可由 GC 回收，但不得被當成已出生 capsule。

**若做錯的代價：** 半提交資料會使出生事件、KA 金鑰與內容位址互相矛盾；原地修改則使 provenance 與遺忘語義失去穩定指涉。

### 4.2 Per-capsule index

**觸發條件：** capsule 寫入。

**行為：** index MUST 在寫入時建立、加密並隨 capsule 不可變；不得建立全域持久明文索引。

**崩潰語義：** index 建立失敗即整顆 capsule 提交失敗；不得提交只有 events blob、卻宣稱可索引的 capsule。

**若做錯的代價：** 後補索引會引入第二個可變真相來源；全域索引則會洩漏已 L2 內容或要求反向清理。

### 4.3 多來源持久衍生品與巢狀 AEAD

**觸發條件：** 衍生品直接使用兩個以上持久來源，且不是 materialize。

**行為：** 衍生品 MUST 以 nested AEAD 綁定全部直接來源；任一來源 DEK 被 L2 後，整個衍生品不可解密，不允許部分解密。

**崩潰語義：** freeze／提交前 MUST 鎖定並重新驗證全部直接來源仍為 Live；任何來源無法確認即整次失敗，不提交弱化版本。

**若做錯的代價：** 可拆分或只檢查部分來源會把 L2 變成可由衍生層繞過的表面承諾。

### 4.4 Provenance 凍結

**觸發條件：** 建立任何持久衍生 capsule。

**行為：**

- provenance MUST 隨建立凍結，不得事後回填。
- 只保存 depth-1 直接來源。
- 若任一直接來源本身有 provenance，MUST 設 `has_upstream_provenance = true`。
- 系統記錄聲明，不驗證聲明為完整世界真相。
- reference 邊單向；不得為 provenance 建反向索引。

**崩潰語義：** provenance 與內容必須同一提交邊界；不得出現有內容無 provenance 或 provenance 指向未提交內容。

**若做錯的代價：** 事後補鏈會要求全域搜尋／反向索引，並讓歷史敘述可被改寫。

### 4.5 Declaration mode 為三態

1. `mandatory_tracked`：機器可強制追蹤的 `{source_key_ids → exposure_modes}`。
2. `optional_human`：使用者主動加入的來源聲明。
3. `materialized_view`：`H(V)`、V 的直接支撐來源及 transform metadata。

不得新增「使用者確認放棄衍生」或「關閉時放棄」第四態；放棄暫態 view 不產生持久事件。

### 4.6 Materialize

**觸發條件：** 使用者明示要求把 process 內仍存活的 view 持久化。

**行為：**

1. Materialize 是讀端到寫端唯一回寫橋樑。
2. 它建立新 capsule、新 `key_id`、新 DEK，並重加密 V 的明文；它是衍生／具現化，不是原地升格。
3. 新 capsule 的 frozen provenance MUST 使用 `materialized_view`，記錄 `H(V)`、直接支撐來源與 transform metadata。
4. 在提交前 MUST 向 KA 鎖定並重新驗證全部支撐來源仍為 Live。
5. 任一來源已 L2、狀態不可確認或在鎖定後失效，整次 materialize MUST 失敗。
6. process 快取的 DEK 不得繞過 KA 驗證。
7. 不回填舊 exposure 邊、不建立 `H(V) → capsule` 反向索引。

**崩潰語義：** 新 capsule 完整提交才算 materialize 成功；失敗不得留下可解析的 promotion alias。KA 驗證到提交之間須有能排除同一來源並行 L2 的鎖定／交易邊界；具體機制留給實作設計，但保證不得降級。

**可達性語義：** 從新 capsule 可往下追溯 `H(V)`；從舊 exposure 不能找到新 capsule。若舊 `H(V)` 已無暫態 view，UI 語義為：「此暫態檢視已不存在，系統不追蹤其後續具現化。」

**若做錯的代價：** 快取 DEK 可在 L2 後 materialize，等同合法復活已遺忘內容；反向回填則違反不可變與無反向索引。

### 4.7 View 的 process lifetime

**觸發條件：** 讀取／推理產生 view。

**行為：**

- view 只存於產生它的 process 記憶體。
- process 正常結束、崩潰、重啟或使用者丟棄 view 時，view 無條件消失。
- 不定義額外的「讀取 session」。
- 不自動 materialize，也不在關閉時提示尚有 N 個 view。
- materialize 窗口同時受兩個條件限制：view 尚存活，且全部支撐來源仍在 KA 中為 Live。

**崩潰語義：** process 崩潰即遺失所有未 materialize view；重啟不恢復。

**若做錯的代價：** 自動保存會把明示特權操作降為默認持久化；關閉提示則製造只有部分情境受保護的假象。

### 4.8 L2 遺忘

**觸發條件：** 使用者對 Live `key_id` 明示執行 L2。

**行為：**

1. KA MUST 銷毀 DEK，並永久保存最小 tombstone：`key_id → Destroyed`。
2. L2 即時、不可逆、整顆生效；無冷卻期、undo、escrow 或反 tombstone。
3. Tombstone 是死亡狀態唯一權威。
4. L2 不掃描或提示當前 view，不列舉應先 materialize 的衍生品。
5. L2 不廣播給其他 process，不回溯清除其記憶體。
6. L2 後，儲存層不得再次解密該 capsule，也不得基於它建立新的持久副本。

**崩潰語義：** 操作必須以 KA 的 Live 或 Destroyed 其中一態收斂；一旦 DEK 銷毀，即使其餘清理未完成也不得恢復。重試 L2 應安全收斂至 Destroyed。

**已解密觀察：** 其他 process 在 L2 前已解密的 view，可完整顯示到被丟棄或該 process 結束。這不是復活；它不能通過 materialize 的 KA 再驗證。

**若做錯的代價：** 若要求全域立即失憶，就必須引入 daemon／IPC 廣播且仍無法物理保證；若允許快取 DEK 新建持久副本，L2 保證即被架空。

### 4.9 KA 與 key identity

**觸發條件：** capsule 出生、讀取、L2 或遷移。

**行為：**

- KA 是唯一永久可變元件，MUST 由 TPM 保護。
- `key_id` MUST 為 CSPRNG 產生的隨機 256-bit 值，永不重用。
- migration control、簽章與 wrapping MUST 使用域分離的金鑰／用途。
- 不得提供 escrow、可攜 KA blob 或可逆銷毀途徑。

**崩潰語義：** KA 更新必須可判定 Live 或 Destroyed；不得因一般 metadata 損毀而把 Destroyed 推回 Live。

**若做錯的代價：** 身分重用會讓 tombstone 誤殺新內容；用途混用會讓遷移權限意外取得解密或簽章能力。

### 4.10 三態查詢回答

對「此內容／key 是否存在過或是否已遺忘」只能給：

1. **存活**：KA 有 Live 記錄。
2. **近期已刪除**：有可用的 Destroyed 與有界審計證據。
3. **歷史不可判定**：本實例無足夠紀錄。

`no_record_in_this_instance` MUST NOT 被表述為「從未存在」。

### 4.11 遷移是終止，不是延續

**觸發條件：** 將 capsule 從一個實例／裝置移交到另一個實例／裝置。

**行為：**

- 遷移語義為來源端流程終止與目的端新生，不是同一身分的連續延伸。
- 遷移採扁平、逐跳模型。
- 不繼承 tombstone、不傳遞跨實例 lineage、無出生屏障。
- 使用有界租約；不得靠永久協調或分散式共識保證全域唯一。
- 結果以 `Finalized(success | failed | unknown)` 表示。
- 個人裝置 weak path 可以存在，但 `attestation_level` MUST 永久標記，不能日後洗成強證明。

**崩潰語義：** 租約逾期或雙方無法確認時收斂為 `Finalized(unknown)`，不得推測成功，也不得無限阻塞等待遠端。

**若做錯的代價：** 把遷移描述成延續會暗示跨實例 tombstone、lineage 與唯一性保證，而本架構刻意不提供這些能力。

### 4.12 有界審計與永久結構

**觸發條件：** 生命週期事件產生或審計資料到期。

**行為：**

- 有界審計窗口為 120 天：90 天輪替 + 30 天 TTL。
- 永久結構只允許三類：
  1. KA tombstones；
  2. 出生記錄；
  3. 死亡／未確認終態記錄。
- 不得新增第五、第六種永久 side table 來補查詢便利性。

**崩潰語義：** TTL／輪替失敗可造成暫時超期保留，但不得提早刪除仍在窗口內的審計資料；恢復後需重新收斂。

**若做錯的代價：** 永久 side table 會逐步重建被明確拒絕的全域歷史、反向索引或跨實例 lineage。

---

## 5. 明確拒絕的設計

以下是負向約束，後續不得以「最佳化」名義復辟：

1. 全域主題、共現或引用反向索引。
2. `H(V) → materialized capsule` 反向查找。
3. searchable encryption。
4. 全域明文索引或明文主題標籤。
5. escrow、可攜 KA blob、DEK 復原機制。
6. 分散式共識或常駐 daemon 作為正確性前提。
7. 反 tombstone、L2 冷卻期、undo、安全網或衍生掃描提醒。
8. L2 廣播與對其他 process 記憶體的回溯抹除。
9. 跨實例 lineage、tombstone 繼承或「遷移即身分延續」。
10. 自動 materialize、關閉時 materialize 或關閉時放棄紀錄。
11. 部分解密多來源衍生品。
12. 以 `no_record_in_this_instance` 宣稱「從未存在」。

---

## 6. 跨層硬介面

第二層設計 MUST 遵守下表；若證明不可實作，必須回到本文件提出變更，不得私下繞路。

| 第一層凍結項 | 對第二層的約束 |
|---|---|
| index per-capsule 且寫入時建立 | 索引樹不得成為全域持久真相或全域明文結構。 |
| 寫端解耦、讀端收斂 | 多行程不得共享可變 capsule 寫入狀態。 |
| 單寫者、每寫入 session 一顆 capsule | 寫入 session 是並行提交單元；不得多寫者共同變更同一 capsule。 |
| view 為 process-lifetime | 跨 process view 分享不存在；若未來需要，須另案定義，不能假裝是同一 view。 |
| L2 不廣播 | 多行程可以保留既有觀察，但所有新持久化都須重新查 KA。 |
| materialize 是唯一讀→寫橋樑 | 任何 reducer／agent 輸出要持久化，都必須走同等前置驗證與新 capsule 提交。 |
| provenance depth-1 且不可變 | reducer 不得展平、改寫或「洗白」上游 provenance。 |
| 無反向索引 | UI 與查詢不得承諾列出「所有引用／所有後續 materialization」。 |
| KA 是持久化權威 | DEK cache 只支援既有觀察，不能授權新的持久副本。 |

---

## 7. 操作前置條件與失敗矩陣

| 操作 | 必要前置條件 | 成功結果 | 失敗語義 |
|---|---|---|---|
| 建立 capsule | 單一寫者、全新 key_id/DEK、完整 index | immutable capsule + 出生記錄 | 無有效半成品；孤兒資料交 GC |
| 建立一般衍生品 | 全部直接來源 Live 且鎖定 | nested-AEAD capsule + frozen provenance | 任一來源失效則整次取消 |
| materialize view | V 尚在 process；全部支撐來源經 KA 再驗證為 Live | 新 DEK 的獨立 capsule + `materialized_view` provenance | 不提交、不建立 alias、不使用 cache 繞過 |
| L2 | 目標 key_id 可定位 | DEK 銷毀 + 永久 tombstone | 重試收斂至 Destroyed；不復原 |
| 讀取 capsule | KA 為 Live 且解密驗證成功 | process 內明文／view | Destroyed 或不可確認均不得新解密 |
| 遷移 | 租約、控制域與 attestation 條件滿足 | 明確 Finalized 結果 | 無法證明時為 unknown，不猜測 |

---

## 8. 並行與崩潰要求

### 8.1 Materialize 與 L2 競爭

必須存在一個由 KA 認可的線性化邊界：

- 若 materialize 先取得全部來源的有效鎖定並提交，之後 L2 不回溯刪除新 capsule；新 capsule 已有自身 DEK。
- 若 L2 先線性化為 Destroyed，materialize 必須失敗，即使 process 仍持有來源明文或 DEK cache。
- 不允許兩邊各自回報成功、但沒有可解釋先後順序。

具體使用交易、鎖、generation 或 compare-and-swap，留待實作設計；語義不可改。

### 8.2 L2 與既有 view

- L2 不使已存在 view 失效。
- 已存在 view 不因此取得持久化權限。
- UI 可同時在一個 process 顯示「已遺忘」，另一 process 顯示早先解密內容；這是已接受的時間邊界，不是狀態不一致 bug。

### 8.3 Process 崩潰

- 所有未 materialize view 消失。
- 不建立恢復日誌來重建 view。
- 若持久操作未越過提交點，重試不得把暫態資料冒充成功提交。

---

## 9. 已知限制與刻意代價

1. **單向追溯：** 新 materialized capsule 知道來處；舊 exposure 不知道去處。
2. **可達性黑洞是明示行為：** `H(V)` 在 process 結束後可成為不可解析的歷史 ID。
3. **無跨會話 view：** 下午產生的 synthesis 若未 materialize，process 結束後即消失。
4. **無全面 L2 預警：** 系統不列出跨會話或當前會話所有受影響衍生品。
5. **已看見的不能被遠端抹除：** L2 無法使其他 process 已解密的內容立刻消失。
6. **歷史回答有限：** 審計窗口外可能只能回答「歷史不可判定」。
7. **遷移不保證全域連續性：** 每跳都有自己的出生與終止；沒有跨實例唯一真相。
8. **查詢能力可降級：** 拒絕全域索引會犧牲便利搜尋，但不能偷偷降級遺忘保證。

---

## 10. 後續另案設計

### 10.1 第二層：索引樹與多行程執行

從 §6 的硬介面出發，至少回答：

- 名詞／動詞／時間三維索引如何在不成為全域持久真相的前提下收斂。
- 多 process 如何各自單寫，讀端如何發現並合併 capsule。
- KA 查詢、鎖定及 materialize/L2 線性化的具體機制。
- DEK cache 的 process 內儲存、清除與權限邊界。
- LanceDB／Tantivy 如何只作可重建能力，而不是死亡權威。

### 10.2 Reducer 設計

OPEN：

- 輸入 capsule／view 的選擇規則。
- 觸發時機及資源界線。
- 如何保存 depth-1 provenance 並避免上游洗白。
- reducer 結果何時只是 view，何時由使用者 materialize。

### 10.3 Materialize UI

OPEN 僅限互動形式，不含語義：

- 誰可觸發、確認文案與進度呈現。
- 來源在確認期間 L2 時如何顯示整次失敗。
- 如何清楚說明「新 capsule 可回溯舊 view，舊 view不能找到新 capsule」。

不得加入自動 materialize、關閉提醒、L2 衍生掃描或第四種 declaration mode。

---

## 11. 實作前驗收清單

- [ ] capsule 格式能把所有語義 metadata 留在加密區，只暴露 GC 必需 header。
- [ ] 半提交 capsule 不會被正常讀取路徑承認。
- [ ] `key_id` 256-bit CSPRNG、不可重用，Live/Destroyed 狀態可原子收斂。
- [ ] L2 後，舊 process view 仍可顯示，但任何 materialize 都被 KA 再驗證拒絕。
- [ ] materialize 與 L2 的競爭有單一可解釋先後順序。
- [ ] 一般多來源衍生品在任一直接來源 L2 後整體不可解密。
- [ ] provenance 為 depth-1、不可變，並正確標記 `has_upstream_provenance`。
- [ ] `materialized_view` 保存 `H(V)`、直接支撐來源與 transform metadata。
- [ ] 不存在 `H(V)` 的全域反向查找。
- [ ] process 結束不提示、不自動保存、也不恢復未 materialize view。
- [ ] 查詢 API 不把 no-record 表述為 never-existed。
- [ ] 遷移 unknown 不被猜成 success／failed，weak attestation 不可升格。
- [ ] 120 天有界審計與三類永久結構沒有被便利性 side table 擴張。
- [ ] 第二層元件若要求違反 §6，會觸發規格回修而非局部例外。

---

## 12. 評審準則

本規格的優先順序是：

1. 保證不可偷偷降級。
2. 崩潰與競爭後仍有唯一可解釋語義。
3. 不用永久全域結構換取查詢便利。
4. 接受能力降級與不可判定，而不虛構更強保證。
5. 把實體限制寫成邊界：已被觀察的明文不能靠儲存層操作抹除。

若後續量測（包含 Tier 1 live-overlap gate）證明某項凍結約束不可行，變更必須列出：原不變量、反例、保證損失、替代方案及遷移影響，再修改本文件；不得先在程式碼裡形成既成事實。
