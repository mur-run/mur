#!/usr/bin/env python3
"""Gate 1: measure CyclicJudge score stability across ordering permutations.

Usage:
    python3 scripts/judge_reliability.py

Requires cc-proxy at 127.0.0.1:8088 (MUR dev default):
    ANTHROPIC_BASE_URL=http://127.0.0.1:8088 ANTHROPIC_API_KEY=cc-proxy python3 scripts/judge_reliability.py
"""
import anthropic, statistics, json

RUBRIC = {"correctness": 0.4, "design": 0.3, "maintainability": 0.2, "security": 0.1}
RUBRIC_DESC = "correctness (40%), design (30%), maintainability (20%), security (10%)"

IMPL_PAIRS = [
    {
        "name": "word_count",
        "a": 'fn word_count(s: &str) -> usize { s.split_whitespace().count() }',
        "b": 'fn word_count(s: &str) -> usize { let mut n = 0; let mut in_word = false; for c in s.chars() { if c.is_whitespace() { in_word = false; } else if !in_word { n += 1; in_word = true; } } n }',
    },
    {
        "name": "is_palindrome",
        "a": 'fn is_palindrome(s: &str) -> bool { let clean: String = s.chars().filter(|c| c.is_alphanumeric()).map(|c| c.to_lowercase().next().unwrap()).collect(); clean == clean.chars().rev().collect::<String>() }',
        "b": 'fn is_palindrome(s: &str) -> bool { let v: Vec<char> = s.chars().filter(|c| c.is_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect(); v.iter().zip(v.iter().rev()).all(|(a, b)| a == b) }',
    },
    {
        "name": "max_in_list",
        "a": 'fn max_val(v: &[i32]) -> Option<i32> { v.iter().copied().max() }',
        "b": 'fn max_val(v: &[i32]) -> Option<i32> { if v.is_empty() { return None; } let mut m = v[0]; for &x in &v[1..] { if x > m { m = x; } } Some(m) }',
    },
]

JUDGE_PROMPT = """\
You are a code reviewer. Score these two Rust implementations on rubric: {rubric}.

Implementation A:
```rust
{impl_a}
```

Implementation B:
```rust
{impl_b}
```

Respond with JSON only: {{"a": <0-10>, "b": <0-10>, "reasoning": "<one sentence>"}}"""

def score_pair(client, impl_a: str, impl_b: str) -> dict:
    resp = client.messages.create(
        model="claude-sonnet-4-6",
        max_tokens=200,
        messages=[{"role": "user", "content": JUDGE_PROMPT.format(
            rubric=RUBRIC_DESC, impl_a=impl_a, impl_b=impl_b
        )}]
    )
    return json.loads(resp.content[0].text.strip())

def main():
    client = anthropic.Anthropic()
    all_deltas, all_flips = [], []

    for pair in IMPL_PAIRS:
        name = pair["name"]
        print(f"\n=== {name} ===")
        # Round 1: A, B
        r1 = score_pair(client, pair["a"], pair["b"])
        # Round 2: B, A (rotated — note which is "A" vs "B" in prompt)
        r2 = score_pair(client, pair["b"], pair["a"])
        # r2["a"] is actually impl_b, r2["b"] is actually impl_a
        # Normalize: impl_a score in each round
        score_a_r1 = r1["a"]
        score_a_r2 = r2["b"]  # impl_a was labeled "B" in round 2
        score_b_r1 = r1["b"]
        score_b_r2 = r2["a"]  # impl_b was labeled "A" in round 2

        delta_a = abs(score_a_r1 - score_a_r2)
        delta_b = abs(score_b_r1 - score_b_r2)
        winner_r1 = "a" if score_a_r1 > score_b_r1 else "b"
        winner_r2 = "a" if score_a_r2 > score_b_r2 else "b"
        flipped = winner_r1 != winner_r2

        print(f"  Round 1 (A,B): A={score_a_r1} B={score_b_r1} winner={winner_r1}")
        print(f"  Round 2 (B,A): A={score_a_r2} B={score_b_r2} winner={winner_r2}")
        print(f"  Delta A: {delta_a:.2f}, Delta B: {delta_b:.2f}, Flipped: {flipped}")
        all_deltas.extend([delta_a, delta_b])
        all_flips.append(flipped)

    mean_delta = statistics.mean(all_deltas)
    flip_rate = sum(all_flips) / len(all_flips)

    print("\n=== GATE 1 SUMMARY ===")
    print(f"Mean score delta across orderings: {mean_delta:.3f} (PASS if ≤ 0.15)")
    print(f"Winner flip rate: {flip_rate:.1%} (PASS if ≤ 20%)")

    passed = mean_delta <= 0.15 and flip_rate <= 0.20
    print(f"\nGATE 1: {'✅ PASS' if passed else '❌ FAIL — redesign judge before Task 7'}")

if __name__ == "__main__":
    main()
