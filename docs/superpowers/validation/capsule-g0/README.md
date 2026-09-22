# Capsule Gate 0 Rust 抽象模型

`G0` 是 Gate 0（可實現性關卡），不是 Go 語言。這個目錄是獨立 Rust crate，不加入 MUR workspace，也不依賴 `mur-*` crates。

這是架構驗證的符號模型，不是 Capsule 核心或 MUR Adapter。Strict 的不可回滾原子 anchor 是模型前提，並未由測試證明真實硬體具有此能力。模型內的 FNV-1a 只產生穩定的符號狀態 ID，不是正式密碼學 commitment。

從專案根目錄執行：

```console
cargo run --manifest-path docs/superpowers/validation/capsule-g0/Cargo.toml --bin capsule-g0 -- --suite abstract-smoke --report /private/tmp/capsule-g0-smoke.json
cargo run --manifest-path docs/superpowers/validation/capsule-g0/Cargo.toml --bin capsule-g0 -- --suite all --report /private/tmp/capsule-g0-all.json
```

runner 會先執行 `cargo test --lib`。`abstract-smoke` 成功退出 0，測試失敗退出 1；`all` 目前會先執行 smoke，再退出 2，因為完整驗收尚未實作。JSON 報告分開列出測試結果與各 gate 的 incomplete／not_run，禁止將退出 2 視為 G0 通過。

模型涵蓋四個抽象持久化階段、復原、衍生來源與 materialize 的部分競爭、冪等、Managed 回滾限制。沒有真實金鑰、加密、磁碟同步、token／批准驗證、隔離、grant/revoke 或任意排程探索。`approved=True` 是外部批准成立的符號前提，不能作為產品 API。模型保留的 request digest 是符號去重資料，不是正式持久化 schema。

截至 2026-09-22：15 個 Rust 測試通過；G0-SM 仍 incomplete，其他 gate 尚未執行。詳見 [契約](../../specs/2026-09-22-capsule-g0-contracts.md)與 [驗收計畫](../../plans/2026-09-22-capsule-g0-validation-plan.md)。
