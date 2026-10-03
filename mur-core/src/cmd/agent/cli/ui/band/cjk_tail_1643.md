你記得沒錯：寫 LSP / ast-grep 那套做法的就是內建技能 **`mur-search`**，它已經裝在你的巢裡，版本也是最新的。

**來源（內建在 mur-core 裡）**
- 定義檔：`mur-core/src/skills/mur_search.yaml`
- 安裝時寫死在 binary 裡，見 `mur-core/src/cmd/sync_cmd/skill_install.rs:89`：
  ```
  ("mur-search", include_str!("../../skills/mur_search.yaml")),
  ```
- 有測試涵蓋，見 `builtin_skill_tests.rs:193`。

**已安裝的版本和 repo 一樣**

| 位置 | 版本 |
|---|---|
| `mur-core/src/skills/mur_search.yaml` | `0.1.0` |
| `~/.mur/skills/mur-search/SKILL.md:3` | `0.1.0` |
| `~/.mur/skills/mur-search/skill.yaml:2` | `0.1.0` |

**它的分工（搜尋階梯）**
1. **Exact** → `rg -F`：錯誤訊息、字串、設定鍵
2. **Symbol** → serena / LSP 的 `find_symbol`、`find_referencing_symbols`；工具清單裡沒有時退回 `rg -w`
3. **Structure** → `ast-grep run -p … -l <lang>`；`which ast-grep` 失敗時退回 `rg -U`
4. **Semantic** → `mur project search`
5. **Cross-project** → `--all`；向量維度不一致時改成逐個專案搜尋

搜尋結果裡的另外兩個（`mur_native_tools.yaml` 和 `2026-06-02-prefer-mur-project-search…` 那份 spec/plan）是比較早的版本，只分「語意搜尋 vs grep」兩層。現在 `mur-native-tools` 把符號和結構的查詢轉給 `mur-search`。

順帶一提：我這個 session 的工具清單裡沒有 `find_symbol`，所以照技能的規則，我在符號層只能退回 `rg -w`。要讓 serena 生效，得把它的 MCP 接到 agent 上。