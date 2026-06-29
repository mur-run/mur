#!/usr/bin/env python3
"""Gate 0: validate agent diversity and tree-sitter extraction reliability."""
import os, re, difflib, statistics
import anthropic
from tree_sitter import Language, Parser
import tree_sitter_rust

APPROACHES = [
    "Prefer functional style: use Iterator combinators, avoid mutable state, compose small functions.",
    "Performance first: static dispatch over dyn, minimize heap allocation, consider cache locality.",
    "Readability first: clear naming, rich error types, full doc comments, test-driven design.",
]

TEST_FUNCTIONS = [
    # (description, prompt to implement)
    ("word_count", "Write a Rust function `fn word_count(s: &str) -> usize` that counts words."),
    ("is_palindrome", "Write a Rust function `fn is_palindrome(s: &str) -> bool` that checks if a string is a palindrome (ignore case, ignore non-alphanumeric)."),
    ("flatten_nested", "Write a Rust function `fn flatten(v: Vec<Vec<i32>>) -> Vec<i32>` that flattens a nested vec."),
]

def get_implementation(client, func_desc: str, approach: str) -> str:
    resp = client.messages.create(
        model="claude-sonnet-4-6",
        max_tokens=1000,
        messages=[{
            "role": "user",
            "content": f"{func_desc}\n\nApproach: {approach}\n\nRespond with ONLY the Rust function, no explanation, no markdown fences."
        }]
    )
    return resp.content[0].text.strip()

def extract_functions(source: str) -> list[str]:
    """Return list of top-level function bodies via tree-sitter."""
    lang = Language(tree_sitter_rust.language())
    parser = Parser(lang)
    tree = parser.parse(source.encode())
    fns = []
    for child in tree.root_node().children:
        if child.type == "function_item":
            fns.append(source[child.start_byte:child.end_byte])
    return fns

def pairwise_similarity(impls: list[str]) -> list[float]:
    scores = []
    for i in range(len(impls)):
        for j in range(i + 1, len(impls)):
            ratio = difflib.SequenceMatcher(None, impls[i], impls[j]).ratio()
            scores.append(ratio)
    return scores

def main():
    client = anthropic.Anthropic()
    results = []

    for func_name, func_desc in TEST_FUNCTIONS:
        print(f"\n=== {func_name} ===")
        impls, extract_errors = [], 0

        for approach in APPROACHES:
            code = get_implementation(client, func_desc, approach)
            fns = extract_functions(code)
            if not fns:
                extract_errors += 1
                print(f"  EXTRACT ERROR for approach: {approach[:40]}...")
                impls.append(code)  # use raw as fallback
            else:
                impls.append(fns[0])

        sims = pairwise_similarity(impls)
        mean_sim = statistics.mean(sims) if sims else 1.0
        any_structural_diff = any(s < 0.70 for s in sims)

        print(f"  Pairwise similarities: {[round(s, 3) for s in sims]}")
        print(f"  Mean similarity: {mean_sim:.3f} (target ≤ 0.60)")
        print(f"  Extract errors: {extract_errors} (target ≤ 5%)")
        print(f"  Structural diff found: {any_structural_diff}")

        results.append({
            "func": func_name,
            "mean_sim": mean_sim,
            "extract_errors": extract_errors,
            "structural_diff": any_structural_diff,
        })

    print("\n=== GATE 0 SUMMARY ===")
    all_mean_sim = statistics.mean(r["mean_sim"] for r in results)
    total_errors = sum(r["extract_errors"] for r in results)
    error_rate = total_errors / (len(TEST_FUNCTIONS) * len(APPROACHES))
    funcs_with_diff = sum(1 for r in results if r["structural_diff"])

    print(f"Overall mean similarity: {all_mean_sim:.3f} (PASS if ≤ 0.60)")
    print(f"Tree-sitter error rate: {error_rate:.1%} (PASS if ≤ 5%)")
    print(f"Functions with structural diff: {funcs_with_diff}/{len(TEST_FUNCTIONS)} (PASS if ≥ 2)")

    passed = all_mean_sim <= 0.60 and error_rate <= 0.05 and funcs_with_diff >= 2
    print(f"\nGATE 0: {'✅ PASS' if passed else '❌ FAIL — do not proceed to Task 1'}")

if __name__ == "__main__":
    main()
