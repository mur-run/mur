use mur_common::parallel::Rubric;

pub fn build_judge_prompt(
    unit_name: &str,
    implementations: &[(String, &str)], // (track_name, source_code)
    rubric: &Rubric,
) -> String {
    let mut prompt = format!(
        "Score these Rust implementations of `{unit_name}` on:\n\
        - Correctness ({:.0}%)\n\
        - Design ({:.0}%)\n\
        - Maintainability ({:.0}%)\n\
        - Security ({:.0}%)\n\n",
        rubric.correctness * 100.0,
        rubric.design * 100.0,
        rubric.maintainability * 100.0,
        rubric.security * 100.0,
    );

    // Present each implementation labeled A, B, C, etc.
    for (idx, (track_name, source)) in implementations.iter().enumerate() {
        let label = (b'A' + idx as u8) as char;
        prompt.push_str(&format!(
            "## Option {label}: {track_name}\n\n```rust\n{source}\n```\n\n",
        ));
    }

    prompt.push_str(
        "Return JSON in this exact format:\n\
        {\"scores\": [{\"label\": \"A\", \"score\": <0-10>, \"reasoning\": \"<one sentence>\"}, ...]}\n\n\
        Score 0–10 on each criterion above. For each option, \
        return ONE entry with the average score and brief reasoning.",
    );

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_contains_all_options() {
        let rubric = Rubric::default();
        let impls = vec![
            ("track-a".to_string(), "fn foo() { 42 }"),
            ("track-b".to_string(), "fn foo() -> i32 { 42 }"),
        ];

        let prompt = build_judge_prompt("test_fn", &impls, &rubric);

        // Check all criteria are mentioned
        assert!(prompt.contains("Correctness"));
        assert!(prompt.contains("Design"));
        assert!(prompt.contains("Maintainability"));
        assert!(prompt.contains("Security"));

        // Check options are labeled A, B
        assert!(prompt.contains("Option A"), "prompt missing 'Option A'");
        assert!(prompt.contains("Option B"), "prompt missing 'Option B'");

        // Check track names appear
        assert!(prompt.contains("track-a"));
        assert!(prompt.contains("track-b"));

        // Check JSON format instruction
        assert!(prompt.contains("JSON"));
        assert!(prompt.contains("\"label\""));
        assert!(prompt.contains("\"score\""));
        assert!(prompt.contains("\"reasoning\""));

        // Check unit name
        assert!(prompt.contains("test_fn"));
    }
}
