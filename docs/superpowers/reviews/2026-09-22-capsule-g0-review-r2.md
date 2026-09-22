# 審核意見 R2 — Capsule G0 可實現性契約與驗收計畫

- **審核對象：** `specs/2026-09-22-capsule-g0-contracts.md`（契約）＋ `plans/2026-09-22-capsule-g0-validation-plan.md`（驗收計畫，198 行）
- **上一輪：** R1 僅審契約，未讀驗收計畫。本輪合併兩份文件重審，並更正 R1 的誤判。
- **結論：** 批准 G0 繼續執行；不批准以契約關閉任何阻塞項；不批准進入 G1。**結論與 R1 相同，但前置待辦從五項縮為三項。**

---

## 0. R1 的更正

R1 的 §2–§10 章節編號全部對應 contracts spec，而 B8 一段寫「該計畫不在本文件內，需要確認該計畫已存在」。計畫存在，量化門檻已在其中。R1 有三處把計畫的既有內容誤記為缺口，本輪撤回。

另有一處結構性更正：R1 把 header 洩漏面歸給 `key_id`，但欄位是 `{capsule_id, key_id}` 成對。移除 `key_id` 而留下 `capsule_id`，依賴圖一樣全見——該選項付出成本卻買不到它宣稱的效果。R1 的裁決題目本身錯了，本輪重新提出。

---

## 1. R1「仍缺」中已被計畫推翻的項目（撤回）

| R1 的要求 | 計畫中的實際內容 | 位置 |
|---|---|---|
| G0-SM「完整有界探索」是多少狀態空間？ | 「最多 4 capsules、3 併發請求、每 history 12 transitions」；門檻「每個可達 transition/crash edge 100% 覆蓋；0 invariant violation」 | `plan:67` |
| G0-CRYPTO 的向量數量與覆蓋率？ | 「至少 100,000 parser mutations」＋「KAT 全過；來源移除／重排／移植／nonce／tag／body／profile 變動全拒絕；0 panic／部分明文輸出」 | `plan:71` |
| G0-DISCOVERY 的 timing 差異門檻？ | 「accuracy ≤ 40%（chance = 33.3%）；超過視為發現洩漏，不以此測試宣稱 constant-time」 | `plan:77` |
| B6「完全沒有提交延遲門檻」 | 有單點門檻：「1k slots、Strict epoch rewrap ＋ erase｜p95 ≤ 30 秒；成功前確實撤除舊 wrappers，不能先回成功」 | `plan:106` |

B6 應改寫而非照收：缺的是第二、第三個資料點（10k／100k）才構成「延遲 vs 存活 slot 數」曲線，不是門檻不存在。

---

## 2. B8 剩餘的真缺口

**Failure routing 只寫了效能一條。** `plan:110` 對 O(N) epoch rewrap 有明確失敗出口：「若 epoch rewrap 的 O(N) 成本不適合本機產品，G0 應產出失敗結論與替代後端提案；不能把這個效能問題推到 G1 再偷偷改金鑰保證」。其餘 11 個 suite 失敗後是回修契約、回修架構、還是降級產品承諾，沒有規則。此項保留為缺口。

---

## 3. 成立且未回應的項目

1. **B1 四狀態空間無交互表。** `contracts §2.1` 只平行列出 `SlotState / EffectiveRead / OperationState / VaultHealth` 四行定義，全文 `Recovering`／`Quarantined` 僅出現 8 次，無組合合法性表。`InDoubt` 定義為「呼叫結果尚無法確認，不是允許推測成功的永久 key 狀態」，但未處理「判斷 InDoubt 所需的 `read_anchor` 自身 InDoubt」的嵌套情形。真漏洞。
2. **B2 六點密碼學問題。** 契約自標候選；`plan:137` 的動作項 `[ ] Reviewer 對候選 §4 提出判斷` 仍未勾選。其中 HKDF `source_DEK` 語義、以及 `ordered_sources` ↔ `layer_index` 綁定兩點最該優先——它們會改 wire format，拖到 G1 就是重做。
3. **B3、B5、B7 評價維持。** B5、B7 接近關閉，B3 可進入執行，與計畫矩陣狀態一致。

---

## 4. §7.1 `ordered_sources` 裁決（本輪已定案）

**裁決：保留 `ordered_sources` 明文於 Header0；§7.2 物理觀察者該列的洩漏無條件接受。**

理由：

1. `ordered_sources` 每項為 `{capsule_id, key_id}`。只移除 `key_id` 不改變圖結構可見性，該選項不改變 §7.2 該列結論。
2. `ordered_sources` 的明文性是 `contracts:157` 各層 AAD 綁定的前提：AAD 為 `encode(["capsule/wrap/g0/v1", Header0, body_digest, layer_index, source_key_id_or_guard])`，而 `contracts:158` 要求的「移除／重排／更換來源、nonce、body 或跨 capsule 移植都必須失敗」正靠此成立。移出明文即失去綁定物；唯一替代是帶 blinding 的 hiding commitment，等同更換 wire format、AAD 與閉包驗證路徑，並新增 commitment 正確性評審項。
3. 代價明確，收益僅是把磁碟觀察者可見的四項 metadata（檔案數、大小、時間、圖結構）減為三項。不值。

已落地的契約修訂：

- `contracts §7.1`：刪去「必須接受或另外修訂 envelope」的二擇一，改為已裁決保留，並記入上述兩點理由與「重新提議移除者須先推翻這兩點」。
- `contracts §7.2` 物理觀察者列：條件句改為無條件陳述——「Header0 明文帶 `ordered_sources[{capsule_id, key_id}]`，依賴圖結構對此觀察者可見；語義與內容不明文。已裁決接受」。
- `contracts §4.1`：新增**凍結範圍界定**。凍結的是結構與 AAD 綁定拓撲（Header0 欄位集合、`ordered_sources` 明文性與排序規則、`body_digest` 產生順序、各層 AAD 組成項與 `layer_index` 語義）；密碼學參數（primitive、HKDF info 字串、nonce 派生、§4.2 上限數值）不在凍結範圍，屬 B2 評審對象，評審後修訂不算破壞凍結。

---

## 5. G0 執行前待辦（三項）

1. 補 B1 四狀態空間交互表與 InDoubt 嵌套恢復規則。
2. 取得 §4 六點的 reviewer 書面意見（`plan:137` 已有此動作項，未執行）。§4 結構已凍結，reviewer 拿到的是既定格式而非條件句。
3. 補 B8 的 failure routing：除效能項外，其餘 11 個 suite 的失敗後行動。

R1 另兩項撤回：「確認計畫存在／補量化門檻」已完成；「確認 MemoryProposalV2 對齊」——計畫 §9 把 MUR 接入劃在 G2，G0 只用合成資料，不構成 G0 的 gate。

---

## 6. 維持不變

G0 執行中仍須：在選定硬體執行 C00–C14 並產出延遲曲線（補 10k／100k 資料點）；執行 E01–E18、M01–M14、隔離測試清單、分頁邊界測試；產出 envelope 二進位測試向量與 parser 拒絕清單；分列五類證據，失敗項不得合併為通過。

只有全部阻塞項有證據且通過，才批准 R4–R10 機制與 G1。B6（硬體／Authority 原子提交）與 B2（密碼學評審）仍是最可能失敗的兩項，優先投入資源。
