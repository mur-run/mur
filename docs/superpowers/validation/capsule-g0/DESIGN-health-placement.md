# 設計：VaultHealth 在 Model 裡的擺法（I09-a3 / b2 / c 共用）

狀態：**設計中，未動工。** 三個切片（a3 `UnsupportedGuarantee` 建構點、b2 identity 輪替、
c schema 版本）都卡在同一件事：契約談的維度，`Model` 沒有表示。先把擺法定下來再寫任何一條。

## 0. 現況實證

`Model` 欄位全貌（`src/lib.rs`）：

```rust
pub struct Model {
    profile: Profile,
    manifests: BTreeMap<Root, Snapshot>,
    hardware_root: Root,
    disk_root: Root,
    pointer: Root,
    session: u64,
}
```

無 `health`、無 `schema_version`、無 `vault_birth`、無 `principal`。

`VaultHealth` 住在 `state_space.rs`，是表格的輸入軸，四態：`Ready` / `Recovering` /
`Quarantined` / `Unsupported`。`lib.rs` 全檔只提到其中一個名字，而且是 `Refusal` 的變體不是
health 的值。

## 1. stored vs derived

**已經有一半是 derived 的，而且是現成先例。** `current()` 就是一個 health 推導：

```rust
// src/lib.rs:128-135
fn current(&self) -> Result<Snapshot, Refusal> {
    let root = self.anchor();
    let snapshot = self.manifests.get(&root).ok_or(Refusal::Quarantined)?;
    if snapshot_digest(snapshot) != root {
        return Err(Refusal::Quarantined);
    }
    Ok(snapshot.clone())
}
```

錨點 manifest 缺失或摘要不符 → `Quarantined`。成功 → 隱含 `Ready`。

所以四態的推導來源盤點：

| 狀態 | Model 裡有沒有推導來源 |
|---|---|
| `Quarantined` | **有** — `lib.rs:130`／`:132` 兩條，已在用 |
| `Ready` | **有** — 上者的補集 |
| `Recovering` | **無** — crate 內零來源 |
| `Unsupported` | **無** — crate 內零來源 |

結論：不要加 `health: VaultHealth` 欄位。那會讓 `Quarantined` 有兩個真相來源（欄位與
`current()`），製造同步問題，而且對缺來源的那兩態毫無幫助 —— 欄位不會憑空生出
`Recovering` 的判準。

**建議：health 維持 derived，`fn health(&self) -> VaultHealth` 作為唯一推導點，`current()`
改為它的呼叫方。** 真正要加的 Model 概念不是 health 本身，而是 `Recovering` 與 `Unsupported`
的輸入：

- `Recovering` ← 需要「恢復中」的可表示狀態（`restore_disk` 之後、`restart` 之前的窗口？現在這
  段是不可觀測的）。
- `Unsupported` ← 需要 c 切片的 `schema_version`：capsule 帶的版本超出本 build 能保證的範圍，
  才叫不支援。**a3 因此依賴 c，不是平行的。**

## 2. advance / publish 繞過 `current()`：有意還是無意

實證，`self.current()` 的呼叫方只有三個：`readable`（:159）、`prepare`（:170）、
`restart`（:284）。`flush` / `advance` / `publish` / `export_disk` / `restore_disk` 都不呼叫。

逐個判讀：

- **`advance`（:253-272）— 像是有意。** 它有自己的三道閘：`session` 相符、
  `anchor() == prepared.base`、`durable`（manifest 摘要自洽）。第三道實際上重跑了 `current()`
  的完整性檢查，只是對 `prepared.root` 而非錨點。不是漏，是換了對象。
- **`publish`（:274-280）— 有意，無害。** `pointer` 在 `lib.rs` 裡**沒有任何讀取方**：
  讀路徑一律走 `anchor()`（:121-126）→ `current()`；`restart`（:285）把 `pointer` 覆寫成
  `anchor()`。`pointer` 是提示不是權威，`stale_pointer_does_not_override_strict_anchor`
  （tests.rs:137）正是在斷言這件事。`publish` 動的是一個不被信任的欄位，閘在 `advance`。
- **`restore_disk`（:298-302）— 有意，它不是操作，是對手。** 它模擬「磁碟被換掉」（損毀、
  回滾、竄改），所以本來就不該有閘；閘在之後的 `restart` → `current()`。
  `old_disk_image_cannot_roll_back_strict_anchor`（tests.rs:34）與
  `corrupt_committed_manifest_is_quarantined` 都依賴它無條件覆寫。
- **`commit`（:304-317）— 閘只在頭，足夠。** 見上兩條。

### 探針（已移除，不留在 tests.rs）

`restore_disk` 不遞增 `session`，所以換碟前 `prepare` 的 `Prepared` 在換碟後、不 restart 的情況下
仍可推進。實跑結果：

| Profile | advance | publish | readable(Z) |
|---|---|---|---|
| Managed | `Ok(())` | `Ok(())` | `Ok(true)` |
| Strict | `Err(CommitConflict)` | `Err(NotCommitted)` | `Err(Quarantined)` |

Strict 守住（`hardware_root` 不隨碟回滾，`anchor != base`）。Managed 放行，但該 `Prepared` 的
`base` 恰好就是回滾後的狀態，所以寫入與其基底一致 —— 這被已宣告的 Managed 回滾上限
（`managed_old_image_demonstrates_declared_rollback_limit`，tests.rs:123）涵蓋，不是新缺口。

**結論：不是 I10/I02 的新缺口。** 本節先前把 `publish`／`restore_disk` 判為漏，是錯的。
health gate 的擺法因此確定為：**只加在讀取路徑（`current()`）＋ `advance` 既有的耐久閘**，
寫入路徑不另加 health gate。§5 第 1 項據此結案。

可選的後續（不阻塞 a3）：把 Managed 的換碟不 restart 行為寫成一條明示斷言，讓「涵蓋在宣告
上限內」從推論變成測試。

## 3. `UnsupportedGuarantee` 的觸發層

`Refusal::UnsupportedGuarantee`（`lib.rs:89`）目前零建構點；`is_permitted_under(Profile)`
零產品碼呼叫方 —— 只有測試在叫。兩者都是懸空的。

若 health 為 derived（§1 的建議），觸發點就是：

```
操作入口 → self.health() → 若 Unsupported 且 self.profile == Strict → Err(UnsupportedGuarantee)
```

亦即建構點在「操作 dispatch 依 health 決定拒絕方式」那一層，不在 `state_space.rs`。表格層繼續
只回 `Legality`，兩層不混。

但這條路徑要能跑，`health()` 得先能回 `Unsupported` —— 見 §1：那需要 c 的 `schema_version`。
**a3 的真正前置是 c，不是「加一個 health 欄位」。**

## 4. 與 b2 / c 的共享結構

三者都是「契約條款落在 Model 沒有表示的維度上」。共用同一套語言：

- 新增的狀態一律進 `Model` 作為**輸入**（`schema_version`、`vault_birth`、`principal`），
  不進 health 作為**結論**。
- 結論一律 derived，單一推導點。
- 表格層（`state_space.rs`）只認 `Legality`；`Refusal` 一律在 `lib.rs` 的操作層產生。

## 5. 待決（動工前必須答）

1. ~~§2 的 `publish` / `restore_disk` 無閘是漏還是有意~~ —— **已結案：有意。** `pointer` 無讀取方、
   `restore_disk` 是對手模擬；探針顯示 Strict 守住、Managed 落在已宣告回滾上限內。health gate 只放讀取路徑。
2. `Recovering` 的判準是什麼？沒有判準就不該保留這個 health 態，或該承認 G0 不模擬它。
3. c 的 `schema_version` 形狀（單一 u64？範圍？），因為 a3 依賴它。

## 6. §2 判讀結果（實證，探針已撤；已修）

判定：**`advance` / `publish` 是漏洞，不是刻意設計。`restore_disk` 是刻意的故障注入工具，不算漏洞。**

探針：Strict、在 `restore_disk(old)` 之後，先斷言 `current() == Err(Quarantined)`，再往下操作：

| 探針 | 結果 |
|---|---|
| advance：換盤造成 quarantine 後，flush + advance 一個換盤前就 prepare 好的操作 | `advance -> Ok(())`，接著 `current() -> Ok` —— quarantine 被**一筆寫入蓋掉並解除** |
| publish：advance 之後、publish 之前換盤 | `publish -> Ok(())`；`pointer` 指向的 root 在磁碟上沒有 manifest |

- advance：`anchor() == prepared.base` 只比對 root，不檢查 manifest 在不在。硬體 anchor 沒被回滾，所以比對通過。違反表格 t3 `(Quarantined, CanCreateAccepted) => Forbidden` 與 N5（Quarantined 凍結，不解決）。也違反 I10：這不是收斂，是被覆寫。
- publish：只比對 `anchor() == prepared.root`，所以把 pointer 發佈到一個不存在的 manifest。這條違反 I02（receipt 對應的線性化點不可解釋）。
- restore_disk / export_disk / flush：呼叫方全在 `src/tests.rs`，它們是在模擬磁碟這一側，不是產品操作。「它們不經過 current()」是對的。

狀態：**修正已套用**（與本段更新在同一個 commit；commit 不能寫入自己的 hash，請以 `git log -- DESIGN-health-placement.md` 查）。

- `advance` 與 `publish` 開頭都先 `self.current()?`，不等 health 的擺法定稿；定稿後只需換成後繼函式，行為不變。
- 回歸測試（修正前兩條都是 `left: Ok(())`，紅燈已驗證）：`strict_advance_refuses_while_quarantined`、`strict_publish_refuses_while_quarantined`。
- 舊測試更正：`strict_refuses_inflight_preparation_after_disk_rollback` 原本斷言 `CommitConflict` / `NotCommitted`，現在改成 `Quarantined`。舊的斷言本身就錯了：那兩種拒絕都暗示「可以重試」，違反 `t1_quarantined_never_implies_try_again_later`。這不是為了讓測試通過而放寬。
- 對 §5 第一問的意義：`current()` 現在同時服務 readable、prepare、advance、publish 四個呼叫點，一處定義全處適用。這是「推導」派的實例，不只是抽象論據。
