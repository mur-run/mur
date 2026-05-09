//! Gate accuracy test: 100 representative queries must achieve >= 85% correct tier.
//!
//! Each case: (query, min_tier, max_tier)
//! min/max bound the acceptable answer (inclusive on both ends).
//! Skip=0, L0=1, L1=2, L2=3

use mur_core::retrieve::gate::{evaluate_query_v2, GateInputs, Tier};

fn tier_ord(t: &Tier) -> u8 {
    match t {
        Tier::Skip => 0,
        Tier::L0 => 1,
        Tier::L1 => 2,
        Tier::L2 => 3,
    }
}

struct Case {
    query: &'static str,
    min: u8,
    max: u8,
}

fn c(query: &'static str, min: u8, max: u8) -> Case {
    Case { query, min, max }
}

fn golden_cases() -> Vec<Case> {
    vec![
        // Skip (0): pure ack / meta / chitchat
        c("ok", 0, 0),
        c("好", 0, 0),
        c("thanks", 0, 0),
        c("對", 0, 0),
        c("嗯", 0, 0),
        c("got it", 0, 0),
        c("sounds good", 0, 0),
        c("alright", 0, 0),
        c("sure", 0, 0),
        c("yep", 0, 0),
        c("no problem", 0, 0),
        c("👍", 0, 0),
        c("🙏", 0, 0),
        c("/help", 0, 0),
        c("/clear", 0, 0),
        c("/model", 0, 0),
        c("/status", 0, 0),
        c("/config", 0, 0),
        c("hi", 0, 0),
        c("hello", 0, 0),
        c("bye", 0, 0),
        c("ok ok ok", 0, 1),
        c("符合", 0, 1),
        c("繼續", 0, 1),
        c("好的", 0, 1),
        // L0 (1): short questions, curiosity
        c("why does this work?", 1, 2),
        c("what is async/await?", 1, 2),
        c("what does serde do?", 1, 2),
        c("為什麼要用 tokio?", 1, 2),
        c("how does LanceDB work?", 1, 2),
        c("explain ownership in Rust", 1, 2),
        c("what is a trait object?", 1, 2),
        c("can you explain lifetimes?", 1, 2),
        c("what is the difference between Arc and Rc?", 1, 2),
        c("why use anyhow?", 1, 2),
        // L1 (2): technical queries, file paths, code identifiers
        c("how do I use tokio::spawn?", 1, 2),
        c("clap derive subcommand example", 1, 2),
        c("the fn handle_event() is broken", 1, 2),
        c("fix the error in store/yaml.rs", 2, 3),
        c("write a test for score_and_rank", 2, 3),
        c("add a --json flag to the list command", 2, 3),
        c("refactor the error handling in inject/hook.rs", 2, 3),
        c("how to implement Display for PatternKind", 1, 2),
        c("search for patterns matching tokio async", 2, 3),
        c("debug the deadlock in supervisor.rs", 2, 3),
        c("update the Cargo.toml to add serde feature", 2, 3),
        c("make the test pass for capture/noise_filter.rs", 2, 3),
        c("the pattern retrieval is returning stale results", 2, 3),
        c("help me implement the adaptive gate scoring function", 2, 3),
        c("I need to add a new CLI subcommand for model registry", 2, 3),
        c("找出 mur-core/src/inject/hook.rs 中的 bug", 2, 3),
        c("implement missing tests for the webhook handler", 2, 3),
        c("create a new pattern for Rust error handling with anyhow", 2, 3),
        c("trace why embed() fails when Ollama is not running", 2, 3),
        c("check the BM25 score calculation in retrieve/scoring.rs", 2, 3),
        // L2 (3): action verbs + long technical + code context
        c("implement the tokio worker pool in murmurd consumer", 2, 3),
        c("build the release binary and run cargo test --workspace", 2, 3),
        c("fix the race condition between heartbeat and event drain in mur-daemon/src/main.rs", 2, 3),
        c("implement progressive disclosure: return L0 index on SessionStart, L1 snippet on UserPromptSubmit", 2, 3),
        c("refactor mur-core/src/cmd/init.rs to split the 1400-line file into submodules under cmd/init/", 2, 3),
        c("add async:true to the Claude Code hook entry written by mur init --hooks in settings.json", 2, 3),
        c("migrate the on-prompt.sh hook to use mur hook prompt --tool claude", 2, 3),
        c("write integration tests for all nine tool stdin schemas in inject/event.rs", 2, 3),
        c("add latency p50/p95/p99 tracking to mur hook stats using duration_ms in the event queue", 2, 3),
        c("deploy the murmurd binary to production and install the launchd plist", 2, 3),
        // CJK action verbs
        c("實作 hook 的 L2 tier 邏輯", 2, 3),
        c("修正 inject/queue.rs 中的 offset 計算錯誤", 2, 3),
        c("新增 mur hook stats 命令顯示每種 tier 的比例", 2, 3),
        c("重構 mur-core/src/inject/index.rs 的 format_l0 函式", 2, 3),
        c("找出並修復 retrieve/gate.rs 中 intent_score 的 regex 錯誤", 2, 3),
        // Edge / boundary
        c("test", 0, 2),
        c("run tests", 0, 2),
        c("cargo test", 1, 2),
        c("cargo build --release", 1, 2),
        c("git log --oneline -10", 1, 2),
        c("ls -la", 0, 1),
        c("pwd", 0, 1),
        c("", 0, 0),
        c("   ", 0, 0),
        c("a", 0, 1),
        c("ab", 0, 1),
        c("???", 0, 1),
        c("...", 0, 1),
        c("TODO", 0, 1),
        // Mixed language
        c("help me 實作 the gate logic in retrieve/gate.rs", 2, 3),
        c("為什麼 score_and_rank 回傳空的結果？", 1, 2),
        c("debug: mur inject hangs on cold start", 2, 3),
        c("fix: mur hook prompt 在 Gemini CLI 下回傳錯誤格式", 2, 3),
        c("add test for 中文 query detection in noise_filter", 2, 3),
    ]
}

#[test]
fn gate_accuracy_on_golden_set() {
    let inputs = GateInputs::default();
    let cases = golden_cases();
    let total = cases.len();
    let mut correct = 0usize;
    let mut failures = Vec::new();

    for case in &cases {
        let outcome = evaluate_query_v2(case.query, &inputs);
        let tier = tier_ord(&outcome.tier);
        if tier >= case.min && tier <= case.max {
            correct += 1;
        } else {
            failures.push(format!(
                "  FAIL: {:?} → tier={} expected=[{},{}] score={:.3}",
                case.query, tier, case.min, case.max, outcome.score
            ));
        }
    }

    let accuracy = correct as f64 / total as f64;
    if !failures.is_empty() {
        println!("\nGolden set failures ({}/{} wrong):", failures.len(), total);
        for f in &failures {
            println!("{f}");
        }
    }
    println!("\nGate accuracy: {correct}/{total} = {:.1}%", accuracy * 100.0);
    assert!(
        accuracy >= 0.85,
        "Gate accuracy {:.1}% < 85% required by spec §7 M0",
        accuracy * 100.0
    );
}
