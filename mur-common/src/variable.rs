//! Variable system — user-defined variables with `{{var_name}}` template syntax.
//!
//! Variables are stored in `~/.mur/variables.yaml` and can be referenced in
//! workflow steps, commands, and descriptions using `{{variable_name}}` syntax.
//!
//! ## Variable Resolution Order
//! 1. CLI overrides (`--var key=value`)
//! 2. Workflow-level defaults (`variables:` section in workflow YAML)
//! 3. Global variables (`~/.mur/variables.yaml`)
//! 4. Environment variables (`$VAR_NAME` → `{{VAR_NAME}}`)

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// A collection of user-defined variables, persisted as `~/.mur/variables.yaml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VariableStore {
    /// Named variable groups (e.g. "default", "production", "staging")
    #[serde(default)]
    pub profiles: BTreeMap<String, BTreeMap<String, String>>,

    /// Active profile name
    #[serde(default = "default_profile")]
    pub active_profile: String,

    /// Global variables (always available, overridden by profile)
    #[serde(default)]
    pub global: BTreeMap<String, String>,
}

fn default_profile() -> String {
    "default".to_string()
}

impl VariableStore {
    /// Path to the variables file.
    pub fn path() -> PathBuf {
        dirs::home_dir()
            .expect("no home dir")
            .join(".mur")
            .join("variables.yaml")
    }

    /// Load from disk, or return empty store if file doesn't exist.
    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read variables file: {}", path.display()))?;
        let store: Self = serde_yaml::from_str(&content)
            .with_context(|| format!("Failed to parse variables YAML: {}", path.display()))?;
        Ok(store)
    }

    /// Save to disk (atomic write).
    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let yaml = serde_yaml::to_string(self)?;
        let tmp = path.with_extension("yaml.tmp");
        fs::write(&tmp, &yaml)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Get the effective variables: global merged with active profile.
    /// Profile values override global values.
    pub fn effective_vars(&self) -> BTreeMap<String, String> {
        let mut vars = self.global.clone();
        if let Some(profile_vars) = self.profiles.get(&self.active_profile) {
            for (k, v) in profile_vars {
                vars.insert(k.clone(), v.clone());
            }
        }
        vars
    }

    /// Set a variable in global scope.
    pub fn set_global(&mut self, name: &str, value: &str) {
        self.global.insert(name.to_string(), value.to_string());
    }

    /// Set a variable in a specific profile.
    pub fn set_profile(&mut self, profile: &str, name: &str, value: &str) {
        self.profiles
            .entry(profile.to_string())
            .or_default()
            .insert(name.to_string(), value.to_string());
    }

    /// Remove a variable from global scope.
    pub fn remove_global(&mut self, name: &str) -> bool {
        self.global.remove(name).is_some()
    }

    /// Remove a variable from a specific profile.
    pub fn remove_profile(&mut self, profile: &str, name: &str) -> bool {
        if let Some(profile_vars) = self.profiles.get_mut(profile) {
            return profile_vars.remove(name).is_some();
        }
        false
    }

    /// Switch active profile.
    pub fn switch_profile(&mut self, profile: &str) {
        self.active_profile = profile.to_string();
        // Ensure the profile entry exists
        self.profiles.entry(profile.to_string()).or_default();
    }

    /// List all profile names.
    pub fn profile_names(&self) -> Vec<&str> {
        self.profiles.keys().map(|s| s.as_str()).collect()
    }
}

// ─── Template Engine ───────────────────────────────────────────────────────

/// Regex pattern for `{{variable_name}}` — allows alphanumeric, underscore, hyphen, dot.
fn var_regex() -> regex_lite::Regex {
    regex_lite::Regex::new(r"\{\{([a-zA-Z_][a-zA-Z0-9_.\-]*)\}\}").unwrap()
}

/// Resolve all `{{var}}` placeholders in a template string.
///
/// Resolution order:
/// 1. `overrides` (CLI --var flags)
/// 2. `workflow_defaults` (from workflow's variables section)
/// 3. `store_vars` (global + active profile from VariableStore)
/// 4. Environment variables
///
/// Unresolved variables are left as-is (with a warning to stderr).
pub fn resolve_variables(
    template: &str,
    overrides: &BTreeMap<String, String>,
    workflow_defaults: &BTreeMap<String, String>,
    store_vars: &BTreeMap<String, String>,
) -> String {
    let re = var_regex();
    let mut result = template.to_string();
    let mut unresolved = Vec::new();

    // We need to iterate and replace; using replace_all with closure
    let resolved = re.replace_all(&result, |caps: &regex_lite::Captures| {
        let var_name = &caps[1];

        // Skip {{input}} — handled by pipeline executor
        if var_name == "input" {
            return caps[0].to_string();
        }

        // Resolution order
        if let Some(val) = overrides.get(var_name) {
            return shell_escape_value(val);
        }
        if let Some(val) = workflow_defaults.get(var_name) {
            return shell_escape_value(val);
        }
        if let Some(val) = store_vars.get(var_name) {
            return shell_escape_value(val);
        }
        // Try environment variable (uppercase version)
        if let Ok(val) = std::env::var(var_name) {
            return shell_escape_value(&val);
        }
        if let Ok(val) = std::env::var(var_name.to_uppercase()) {
            return shell_escape_value(&val);
        }

        unresolved.push(var_name.to_string());
        caps[0].to_string() // leave as-is
    });

    result = resolved.into_owned();

    if !unresolved.is_empty() {
        eprintln!(
            "⚠ Unresolved variables: {}",
            unresolved
                .iter()
                .map(|v| format!("{{{{{}}}}}", v))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    result
}

/// Shell-safe escaping for variable values used in commands.
fn shell_escape_value(val: &str) -> String {
    // For display/description contexts, return as-is.
    // Shell commands get escaped by the pipeline executor via shell-escape.
    val.to_string()
}

/// Extract all `{{var}}` names from a template string (excluding `{{input}}`).
pub fn extract_variable_names(template: &str) -> Vec<String> {
    let re = var_regex();
    let mut names: Vec<String> = re
        .captures_iter(template)
        .map(|c| c[1].to_string())
        .filter(|n| n != "input")
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Collect all variable references from a workflow's steps and descriptions.
pub fn collect_workflow_variables(workflow: &crate::workflow::Workflow) -> Vec<String> {
    let mut all_names = Vec::new();

    // From description
    all_names.extend(extract_variable_names(&workflow.description));

    // From content
    all_names.extend(extract_variable_names(&workflow.content.as_text()));

    // From steps
    for step in &workflow.steps {
        all_names.extend(extract_variable_names(&step.description));
        if let Some(ref cmd) = step.command {
            all_names.extend(extract_variable_names(cmd));
        }
    }

    all_names.sort();
    all_names.dedup();
    all_names
}

/// Build workflow defaults map from a workflow's `variables` section.
pub fn workflow_defaults_map(workflow: &crate::workflow::Workflow) -> BTreeMap<String, String> {
    let mut defaults = BTreeMap::new();
    for v in &workflow.variables {
        if let Some(ref dv) = v.default_value {
            defaults.insert(v.name.clone(), dv.clone());
        }
    }
    defaults
}

/// Parse CLI `--var key=value` pairs into a map.
pub fn parse_var_overrides(pairs: &[String]) -> Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for pair in pairs {
        let (key, value) = pair
            .split_once('=')
            .with_context(|| format!("Invalid --var format '{}', expected key=value", pair))?;
        map.insert(key.trim().to_string(), value.trim().to_string());
    }
    Ok(map)
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_variable_names() {
        let names = extract_variable_names("Deploy {{app_name}} to {{site_url}} with {{input}}");
        assert_eq!(names, vec!["app_name", "site_url"]);
    }

    #[test]
    fn test_extract_no_variables() {
        let names = extract_variable_names("No variables here");
        assert!(names.is_empty());
    }

    #[test]
    fn test_resolve_from_overrides() {
        let overrides = BTreeMap::from([("name".into(), "myapp".into())]);
        let result = resolve_variables(
            "Deploy {{name}}",
            &overrides,
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert_eq!(result, "Deploy myapp");
    }

    #[test]
    fn test_resolve_priority_order() {
        let overrides = BTreeMap::from([("x".into(), "override".into())]);
        let defaults = BTreeMap::from([("x".into(), "default".into())]);
        let store = BTreeMap::from([("x".into(), "global".into())]);

        let result = resolve_variables("{{x}}", &overrides, &defaults, &store);
        assert_eq!(result, "override");

        let result = resolve_variables("{{x}}", &BTreeMap::new(), &defaults, &store);
        assert_eq!(result, "default");

        let result = resolve_variables("{{x}}", &BTreeMap::new(), &BTreeMap::new(), &store);
        assert_eq!(result, "global");
    }

    #[test]
    fn test_resolve_leaves_input_alone() {
        let result = resolve_variables(
            "echo {{input}} and {{name}}",
            &BTreeMap::from([("name".into(), "test".into())]),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert_eq!(result, "echo {{input}} and test");
    }

    #[test]
    fn test_resolve_unresolved_left_as_is() {
        let result = resolve_variables(
            "{{known}} and {{unknown}}",
            &BTreeMap::from([("known".into(), "yes".into())]),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert_eq!(result, "yes and {{unknown}}");
    }

    #[test]
    fn test_resolve_env_var() {
        // SAFETY: test-only, single-threaded context
        unsafe {
            std::env::set_var("MUR_TEST_VAR_XYZ", "from_env");
        }
        let result = resolve_variables(
            "{{MUR_TEST_VAR_XYZ}}",
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert_eq!(result, "from_env");
        // SAFETY: test-only, single-threaded context
        unsafe {
            std::env::remove_var("MUR_TEST_VAR_XYZ");
        }
    }

    #[test]
    fn test_parse_var_overrides() {
        let pairs = vec!["name=myapp".into(), "url=https://example.com".into()];
        let map = parse_var_overrides(&pairs).unwrap();
        assert_eq!(map.get("name").unwrap(), "myapp");
        assert_eq!(map.get("url").unwrap(), "https://example.com");
    }

    #[test]
    fn test_parse_var_overrides_invalid() {
        let pairs = vec!["bad_format".into()];
        assert!(parse_var_overrides(&pairs).is_err());
    }

    #[test]
    fn test_variable_store_effective_vars() {
        let store = VariableStore {
            global: BTreeMap::from([
                ("site".into(), "global.com".into()),
                ("db".into(), "global-db".into()),
            ]),
            profiles: BTreeMap::from([(
                "production".into(),
                BTreeMap::from([("site".into(), "prod.com".into())]),
            )]),
            active_profile: "production".into(),
        };
        let vars = store.effective_vars();
        assert_eq!(vars.get("site").unwrap(), "prod.com"); // profile overrides global
        assert_eq!(vars.get("db").unwrap(), "global-db"); // global fallback
    }

    #[test]
    fn test_multiple_same_variable() {
        let overrides = BTreeMap::from([("x".into(), "val".into())]);
        let result = resolve_variables(
            "{{x}} and {{x}} again",
            &overrides,
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert_eq!(result, "val and val again");
    }
}
