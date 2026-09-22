# G0 requirement-to-vector map (I01–I10)

逐句核對，非貼標。每條 invariant 拆成子句，各自指名 vector 或宣告缺口。
來源：`specs/2026-09-22-capsule-g0-contracts.md:64-73`（I01–I10 原文）。
覆蓋目標：G0-SM（plan:67）與 G0-CONFORMANCE（plan:80 requirement-to-vector map 100%）。

狀態符號：`✓` A0 已覆蓋 ·『部分』子句未全覆蓋 ·『缺』零覆蓋 ·『界外』A0 表達不了、已宣告移交

| ID | 子句 | 狀態 | vector |
|---|---|---|---|
| I01 | (a) 無有效 grant → 不發布新明文 | 缺 | 動作字母表無 Grant/Revoke（lib.rs:15）；approval 只是 `prepare` 入參，非 grant。**待 L1** |
| I01 | (b) 狀態不可確認 → 不發布新明文 | ✓ | `t1_recovering_publishes_no_new_plaintext_and_judges_no_ancestor`、`t1_forbids_vault_quarantined_everywhere_except_quarantined`、`n5_quarantine_freezes_rather_than_aborting_a_possibly_committed_operation`、`n6_pending_operation_never_changes_a_slot_state` |
| I02 | (a) 一個成功操作只有一個 commit receipt | ✓ | `lost_reply_retry_returns_original_receipt_without_resurrection` |
| I02 | (b) 一個可解釋線性化點（併發下） | 部分 | 僅 `both_derive_erase_serializations_have_no_post_erase_read` 手寫兩序；無排程探索。**待 L0** |
| I03 | (a) 未被 anchor 選中的 prepared 不可出生 | ✓ | `unflushed_snapshot_cannot_advance_anchor`(NotDurable)、`erase_recovery_on_each_side_of_commit_anchor`(cut<2)、`restart_cannot_redeem_uncommitted_preparation`(ExpiredPreparation) |
| I03 | (b) 不可兌換 key | 部分 | 以 `readable()` 代理，模型無獨立 key 兌換路徑；表達力缺口，非漏測。**記於本表** |
| I04 | (a) 舊 active_pointer 不得恢復解密 | ✓ | `stale_pointer_does_not_override_strict_anchor`、`old_disk_image_cannot_roll_back_strict_anchor`、反向控制 `negative_control_detects_rollbackable_strict_anchor` |
| I04 | (b) 舊 D / 已銷毀 key identity | ✓ | `destroyed_key_identity_cannot_be_reused` |
| I04 | (c) 舊 WAL、舊 envelope 作為獨立 artifact | 界外 | 模型只有單一 `DiskImage`，無 WAL/envelope 分層。契約已指派 G0-HW（plan:70「舊 snapshot/WAL、舊 envelope、直接低層 unseal」）。A0 宣告移交 |
| I05 | 必要來源失效 → 依賴者不得新解密／新持久化 | ✓ | `source_erasure_blocks_transitive_derivations`（B、C 遞移不可讀 + 新 D 回 SourceUnavailable） |
| I06 | (a) 來源先失效的 materialize 失敗 | ✓ | `independent_save_requires_approval_and_live_sources_at_commit`（CommitConflict + SourceUnavailable） |
| I06 | (b) 已先提交的獨立副本不被回溯刪除 | ✓ | `approved_save_before_erasure_survives` |
| I07 | (a) 同 ID 同輸入回原 receipt／目前終態 | ✓ | `lost_reply_retry_returns_original_receipt_without_resurrection` |
| I07 | (b) 異輸入拒絕、不重複副作用 | ✓ | 同上（IdempotencyConflict 分支） |
| I08 | (a) 重啟不恢復未提交 View | ✓ | `restart_cannot_redeem_uncommitted_preparation` |
| I08 | (b) 重啟不恢復未兌現批准 | 缺 | approval 無消耗狀態、不持久化，無法表達「重啟後批准仍可兌現」的反例。**待 L1** |
| I08 | (c) 重啟不恢復 session key | 界外 | 模型無 session key 概念。S0/H0（G0-MEM、G0-ISOLATION） |
| I09 | schema / profile / identity 變動不得靜默降低保證 | **缺** | **零覆蓋，且非單一缺口 —— 拆六切片，見下表。** `managed_old_image_demonstrates_declared_rollback_limit` 只證明 Managed 的弱保證是「已宣告」，不是「變動」。無 schema 版本、無 profile 遷移、無 identity 輪替 |

### I09 切片拆解

上一版本記為「A0 現在就能做、唯一不需任何前置的整條缺口」—— **誤判，已撤回。** I09 不是一條缺口，是六個切片，前置條件各異。

| 切片 | 內容 | 前置 | 備註 |
|---|---|---|---|
| **a1** | t1 的 Unsupported×Readable 改判 `ManagedOnly`；新增 `is_permitted_under(Profile)` | 無 | 範圍**已縮為 2/2**（表格層）。**已寫，未驗**（sandbox 拒 exec test binary） |
| **a3** | `UnsupportedGuarantee` 的建構點 —— 讓某條產品碼路徑真的回傳它 | **需加 Model 概念** | 原列為 a1 第三腿，**誤分類，已撤回**。`grep` 實證：`Refusal::UnsupportedGuarantee` 零建構點（僅 `lib.rs:89` 宣告）、`is_permitted_under` 零產品碼呼叫方、`Model` 無 health 表示。不是接線，是加概念，與 b2／c 同類 |
| a2 | profile 遷移 vector（Managed → Strict 與反向） | **需加 Model 概念** | **誤分類，前置改判。** `profile` 只在 `Model::new` 寫入一次（`src/lib.rs` `pub fn new(profile: Profile)`），全 crate 無遷移轉移；契約只說「沒有 Strict → Managed 的恢復轉移」（contracts.md:43）。先有遷移 API 才有 vector，歸入 a3/b2/c 同一設計批。 |
| b1 | `KeyIdentityReused` 覆蓋 | 無 | **✓ 已收。** 「零測試」原判有誤：`destroyed_key_identity_cannot_be_reused` 早已覆蓋 Destroyed 分支（僅 Strict）。新增 `capture_onto_existing_key_is_key_identity_reused`（Live 分支，兩 profile）。紅綠：拔守衛 → 兩條皆 FAILED。 |
| b2 | identity 輪替（vault_birth／principal 需為狀態，非欄位） | **需加 Model 概念** | 先設計，不倉促加 |
| c | schema 版本變動（crate 內 `schema` 目前零出現） | **需加 Model 概念** | 先設計，不倉促加 |

**a1 擋住 L0。** 現行 t1/t2/t3 是 profile-blind，`(V::Unsupported, _) => Legal` 這條 arm 對 Strict 是錯的；L0 探索器若建在這個假設上，整個狀態空間會以錯誤的合法性邊界展開。
| I10 | (a) 已提交物件完整 → 恢復收斂到相同 root | ✓ | `recovery_converges_to_identical_root_on_each_side_of_anchor`：兩 profile × 四切點，斷言 anchor／pointer／current() digest 三者皆等於預期 root。紅綠：restart 不重設 pointer → FAILED。 |
| I10 | (b) 物件損毀 → 隔離 | ✓ | `corrupt_committed_manifest_is_quarantined` |
| I10 | (c) 後端不可用 → 有界回錯，不永久卡住 | 缺 | `n2_timeout_keeps_in_doubt_and_never_presumes_aborted`、`n5` 證明「凍結」，未證明「有界」。liveness 側無 vector。**需時鐘 → S0** |

## 結算

- 完全覆蓋：I05、I06、I07（3 條）
- 部分覆蓋：I01、I02、I03、I04、I08、I10（6 條）
- 零覆蓋：**I09**（1 條，內含 6 切片）
- 缺口歸屬：L1 字母表 2 項 · L0 探索器 1 項 · A0 可立即做 3 項（I09-a1/a2/b1）· 需加 Model 概念 2 項（I09-b2/c）· S0/H0 界外 3 項（已宣告）

## 待辦（依可動性排序）

- [ ] **I09-a1：t1/t3 加 Profile 維度 —— L0 的前置，先做**
- [x] I10(a)：root 相等斷言（`recovery_converges_to_identical_root_on_each_side_of_anchor`）
- [x] restore_disk 繞道探針落成 vector：`strict_refuses_inflight_preparation_after_disk_rollback`（安全屬性）、`managed_inflight_preparation_after_disk_rollback_stays_within_declared_limit`（邊界記錄）
- [ ] L0 註記：`restore_disk` 不遞增 session，是 health 推導盲點；多重 restore／更舊 anchor／restore 後再 prepare 交給探索器
- [x] I09-b1
- [ ] I09-a2：改判需加 Model 概念（遷移 API），併入 a3/b2/c 設計批
- [ ] I09-b2、I09-c：需先加 Model 概念（vault_birth／principal／schema 為狀態），先設計
- [ ] I01(a)、I08(b)：L1 加入 Grant/Revoke 與可消耗 approval 後補
- [ ] I02(b)：L0 探索器完成後補
- [ ] I03(b)、I04(c)、I08(c)、I10(c)：界外，於 G0 報告的 S0/H0 分區聲明，不得算入 A0 覆蓋率分母
