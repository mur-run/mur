//! Task 3: turn a recorded [`Run`] into a Playwright `.spec.ts` file
//! (SPEC — `mur browser export`).
//!
//! Only `mode: test` runs have assertions worth replaying as a spec;
//! `mode: automation` runs are rejected rather than silently emitting a
//! spec with no expectations.

use crate::recorder::{Action, Mode, Run, Step};

/// Render `run` as a Playwright TypeScript test spec.
///
/// Errors if `run.mode` is [`Mode::Automation`] — automation runs carry no
/// assertions, so a generated spec would silently pass on nothing.
pub fn to_spec_ts(run: &Run) -> anyhow::Result<String> {
    anyhow::ensure!(
        run.mode == Mode::Test,
        "cannot export run {:?}: mode is automation, not test (no assertions to replay)",
        run.name
    );

    let mut out = String::new();
    out.push_str("import { test, expect } from '@playwright/test';\n\n");
    out.push_str(&format!(
        "test('{}', async ({{ page }}) => {{\n",
        escape_ts_string(&run.name)
    ));
    for step in &run.steps {
        out.push_str(&format!("  // {}\n", escape_comment(&step.intent)));
        out.push_str(&render_step(step)?);
    }
    out.push_str("});\n");
    Ok(out)
}

/// One step's Playwright statement(s), indented for the `test(...)` body.
fn render_step(step: &Step) -> anyhow::Result<String> {
    let value = step.value.as_deref().map(render_value_expr);
    let locator = step
        .action
        .needs_locator()
        .then(|| {
            step.locators.first().ok_or_else(|| {
                anyhow::anyhow!(
                    "step {} ({:?}) needs a locator but has none",
                    step.step,
                    step.action
                )
            })
        })
        .transpose()?
        .map(|l| locator_expr(l));

    let line = match step.action {
        Action::Goto => {
            let value = value
                .ok_or_else(|| anyhow::anyhow!("step {} (goto) has no url value", step.step))?;
            format!("  await page.goto({value});\n")
        }
        Action::Click => format!("  await {}.click();\n", locator.unwrap()),
        Action::Fill => {
            let value = value
                .ok_or_else(|| anyhow::anyhow!("step {} (fill) has no text value", step.step))?;
            format!("  await {}.fill({value});\n", locator.unwrap())
        }
        Action::Select => {
            let value =
                value.ok_or_else(|| anyhow::anyhow!("step {} (select) has no value", step.step))?;
            format!("  await {}.selectOption({value});\n", locator.unwrap())
        }
        Action::Press => {
            let value = value
                .ok_or_else(|| anyhow::anyhow!("step {} (press) has no key value", step.step))?;
            format!("  await {}.press({value});\n", locator.unwrap())
        }
        Action::Hover => format!("  await {}.hover();\n", locator.unwrap()),
        Action::AssertVisible => {
            format!("  await expect({}).toBeVisible();\n", locator.unwrap())
        }
        Action::AssertText => {
            let value = value.ok_or_else(|| {
                anyhow::anyhow!("step {} (assert_text) has no expected text", step.step)
            })?;
            format!(
                "  await expect({}).toHaveText({value});\n",
                locator.unwrap()
            )
        }
        Action::AssertValue => {
            let value = value.ok_or_else(|| {
                anyhow::anyhow!("step {} (assert_value) has no expected value", step.step)
            })?;
            format!(
                "  await expect({}).toHaveValue({value});\n",
                locator.unwrap()
            )
        }
    };
    Ok(line)
}

/// A step's `value`, rendered as a TS expression: a secret placeholder
/// becomes `process.env.<SITE>_<KEY>`, everything else a quoted string
/// literal. Never emits the placeholder text or a plaintext secret.
fn render_value_expr(value: &str) -> String {
    match crate::broker::parse_placeholder(value) {
        Some((site, key)) => format!("process.env.{}_{}", env_ident(site), env_ident(key)),
        None => format!("'{}'", escape_ts_string(value)),
    }
}

/// Uppercase, non-alphanumeric-to-`_` transform for a `process.env.NAME`
/// segment (site or key half of a `{{secret:<site>/<key>}}` placeholder).
fn env_ident(part: &str) -> String {
    part.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Convert a recorded locator string into a Playwright `page.getBy*`/
/// `page.locator` call.
fn locator_expr(locator: &str) -> String {
    match crate::locator::Locator::parse(locator) {
        Ok(crate::locator::Locator::Role {
            role,
            name: Some(n),
        }) => format!(
            "page.getByRole('{}', {{ name: '{}' }})",
            escape_ts_string(&role),
            escape_ts_string(&n)
        ),
        Ok(crate::locator::Locator::Role { role, name: None }) => {
            format!("page.getByRole('{}')", escape_ts_string(&role))
        }
        Ok(crate::locator::Locator::TestId(id)) => {
            format!("page.getByTestId('{}')", escape_ts_string(&id))
        }
        Ok(crate::locator::Locator::Text(text)) => {
            format!("page.getByText('{}')", escape_ts_string(&text))
        }
        Ok(crate::locator::Locator::Label(label)) => {
            format!("page.getByLabel('{}')", escape_ts_string(&label))
        }
        Ok(crate::locator::Locator::Css(css)) => {
            format!("page.locator('{}')", escape_ts_string(&css))
        }
        // A locator that fails to parse still needs to appear in the spec
        // rather than aborting the whole export; fall back to `.locator()`
        // with the raw string escaped.
        Err(_) => format!("page.locator('{}')", escape_ts_string(locator)),
    }
}

/// Escape `'` and `\` for a single-quoted TS string literal.
fn escape_ts_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Escape a `// comment` body: strip newlines so intent text can never break
/// out of the line comment.
fn escape_comment(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::from_yaml;

    fn run_with_steps(mode: Mode, steps: Vec<Step>) -> Run {
        Run {
            name: "checkout-flow".to_string(),
            mode,
            profile: None,
            recorded_at: chrono::Utc::now(),
            steps,
        }
    }

    fn step(step_no: u32, action: Action) -> Step {
        Step {
            step: step_no,
            intent: "do a thing".to_string(),
            intent_auto: false,
            action,
            value: None,
            locators: Vec::new(),
            healed: false,
            last_hit: 0,
            ref_at_record: None,
        }
    }

    #[test]
    fn emits_spec_header_and_goto_and_fill() {
        let mut goto = step(1, Action::Goto);
        goto.value = Some("https://example.com/login".into());
        let mut fill = step(2, Action::Fill);
        fill.value = Some("hello@example.com".into());
        fill.locators = vec!["role:textbox[name=\"Email\"]".into()];
        let mut assert = step(3, Action::AssertVisible);
        assert.locators = vec!["testid:welcome-banner".into()];

        let run = run_with_steps(Mode::Test, vec![goto, fill, assert]);
        let spec = to_spec_ts(&run).expect("test-mode run exports");

        assert!(spec.contains("import { test, expect } from '@playwright/test';"));
        assert!(spec.contains("test('checkout-flow', async ({ page }) => {"));
        assert!(spec.contains("await page.goto('https://example.com/login');"));
        assert!(spec.contains(
            "await page.getByRole('textbox', { name: 'Email' }).fill('hello@example.com');"
        ));
        assert!(spec.contains("await expect(page.getByTestId('welcome-banner')).toBeVisible();"));
    }

    #[test]
    fn secret_placeholder_becomes_env_var_never_leaks_raw_value() {
        let mut fill = step(1, Action::Fill);
        fill.value = Some("{{secret:acme/PASSWORD}}".into());
        fill.locators = vec!["role:textbox[name=\"Password\"]".into()];

        let run = run_with_steps(Mode::Test, vec![fill]);
        let spec = to_spec_ts(&run).expect("test-mode run exports");

        assert!(spec.contains("process.env.ACME_PASSWORD"));
        assert!(!spec.contains("{{secret:"));
        assert!(!spec.contains("acme/PASSWORD}}"));
    }

    #[test]
    fn secret_placeholder_with_lowercase_site_and_key_roundtrips_env_var() {
        let mut fill = step(1, Action::Fill);
        fill.value = Some("{{secret:my-site/api_key}}".into());
        fill.locators = vec!["role:textbox[name=\"Key\"]".into()];

        let run = run_with_steps(Mode::Test, vec![fill]);
        let spec = to_spec_ts(&run).expect("test-mode run exports");

        assert!(spec.contains("process.env.MY_SITE_API_KEY"));
        assert!(!spec.contains("{{secret:"));
    }

    #[test]
    fn automation_mode_is_rejected() {
        let run = run_with_steps(Mode::Automation, vec![step(1, Action::Goto)]);
        let err = to_spec_ts(&run).expect_err("automation runs have no assertions to export");
        assert!(err.to_string().contains("automation"));
    }

    #[test]
    fn fixture_roundtrip_exports_all_nine_actions() {
        let yaml = include_str!("../tests/fixtures/run-roundtrip.yaml");
        let run = from_yaml(yaml).expect("fixture parses");
        let spec = to_spec_ts(&run).expect("fixture is mode: test");

        assert!(spec.contains("await page.goto("));
        assert!(spec.contains(".click();"));
        assert!(spec.contains(".fill("));
        assert!(spec.contains(".selectOption("));
        assert!(spec.contains(".press("));
        assert!(spec.contains(".hover();"));
        assert!(spec.contains("toBeVisible();"));
        assert!(spec.contains("toHaveText("));
        assert!(spec.contains("toHaveValue("));
    }

    #[test]
    fn escapes_single_quotes_and_backslashes_in_values() {
        let mut fill = step(1, Action::Fill);
        fill.value = Some(r"O'Brien\path".into());
        fill.locators = vec!["role:textbox[name=\"Name\"]".into()];

        let run = run_with_steps(Mode::Test, vec![fill]);
        let spec = to_spec_ts(&run).expect("test-mode run exports");

        assert!(spec.contains(r"O\'Brien\\path"));
    }
}
