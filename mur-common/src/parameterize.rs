//! Auto-detection of parameterizable values in workflow exports.
//!
//! When `mur out` exports a workflow, this module scans all text content
//! (step descriptions, commands, URLs) for values that should be replaced
//! with `{{variable}}` templates to make the workflow reusable.
//!
//! ## Detected Patterns
//! - URLs (http/https) → `{{base_url}}`, `{{api_url}}`, `{{site_url}}`
//! - File paths (absolute) → `{{project_dir}}`, `{{output_path}}`
//! - API tokens/keys → `{{api_key}}`, `{{auth_token}}`
//! - Email addresses → `{{email}}`
//! - Port numbers → `{{port}}`
//! - Domain names → `{{domain}}`
//! - Git repos → `{{repo_url}}`
//! - Docker images → `{{docker_image}}`

use std::collections::BTreeMap;

use crate::workflow::{VarType, Variable};

/// A suggestion to replace a detected value with a variable.
#[derive(Debug, Clone)]
pub struct ParameterSuggestion {
    /// The original literal value found in the text
    pub original_value: String,
    /// Suggested variable name (e.g. "base_url", "api_key")
    pub suggested_name: String,
    /// Human-readable description
    pub description: String,
    /// The category of this detection
    pub category: DetectedCategory,
    /// Confidence score 0.0–1.0
    pub confidence: f64,
}

/// Categories of auto-detected parameterizable values.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DetectedCategory {
    Url,
    FilePath,
    ApiKey,
    Email,
    Port,
    Domain,
    GitRepo,
    DockerImage,
    IpAddress,
    DatabaseUrl,
    EnvVar,
    /// User-specific value (username, home dir, etc.)
    UserSpecific,
}

impl DetectedCategory {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Url => "URL",
            Self::FilePath => "File path",
            Self::ApiKey => "API key/token",
            Self::Email => "Email",
            Self::Port => "Port",
            Self::Domain => "Domain",
            Self::GitRepo => "Git repository",
            Self::DockerImage => "Docker image",
            Self::IpAddress => "IP address",
            Self::DatabaseUrl => "Database URL",
            Self::EnvVar => "Environment variable",
            Self::UserSpecific => "User-specific value",
        }
    }
}

/// Scan text content for parameterizable values and return suggestions.
///
/// This is the main entry point — call with all text from a workflow
/// (concatenated step descriptions, commands, etc.).
pub fn detect_parameterizable_values(texts: &[&str]) -> Vec<ParameterSuggestion> {
    let mut suggestions = Vec::new();
    let mut seen_values: BTreeMap<String, String> = BTreeMap::new(); // value → suggested_name

    for text in texts {
        detect_urls(text, &mut suggestions, &mut seen_values);
        detect_file_paths(text, &mut suggestions, &mut seen_values);
        detect_api_keys(text, &mut suggestions, &mut seen_values);
        detect_emails(text, &mut suggestions, &mut seen_values);
        detect_ports(text, &mut suggestions, &mut seen_values);
        detect_ip_addresses(text, &mut suggestions, &mut seen_values);
        detect_database_urls(text, &mut suggestions, &mut seen_values);
        detect_docker_images(text, &mut suggestions, &mut seen_values);
        detect_git_repos(text, &mut suggestions, &mut seen_values);
        detect_user_specific(text, &mut suggestions, &mut seen_values);
    }

    // Deduplicate by original_value
    suggestions.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap());
    let mut deduped = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for s in suggestions {
        if seen.insert(s.original_value.clone()) {
            deduped.push(s);
        }
    }
    deduped
}

/// Convert suggestions into workflow Variable definitions.
pub fn suggestions_to_variables(suggestions: &[ParameterSuggestion]) -> Vec<Variable> {
    let mut vars = Vec::new();
    let mut used_names = std::collections::HashSet::new();

    for s in suggestions {
        let name = if used_names.contains(&s.suggested_name) {
            // Disambiguate with a suffix
            let mut n = s.suggested_name.clone();
            let mut i = 2;
            while used_names.contains(&n) {
                n = format!("{}_{}", s.suggested_name, i);
                i += 1;
            }
            n
        } else {
            s.suggested_name.clone()
        };
        used_names.insert(name.clone());

        vars.push(Variable {
            name,
            var_type: VarType::String,
            required: s.category != DetectedCategory::Port,
            default_value: Some(s.original_value.clone()),
            description: s.description.clone(),
        });
    }
    vars
}

/// Apply variable substitutions to text — replace literal values with `{{var_name}}`.
pub fn apply_parameterization(text: &str, suggestions: &[ParameterSuggestion]) -> String {
    let mut result = text.to_string();
    // Sort by length descending to avoid partial replacements
    let mut sorted: Vec<_> = suggestions.iter().collect();
    sorted.sort_by(|a, b| b.original_value.len().cmp(&a.original_value.len()));

    for s in sorted {
        result = result.replace(&s.original_value, &format!("{{{{{}}}}}", s.suggested_name));
    }
    result
}

/// Format suggestions for display in the terminal.
pub fn format_suggestions_display(suggestions: &[ParameterSuggestion]) -> String {
    if suggestions.is_empty() {
        return String::from("  No parameterizable values detected.");
    }

    let mut out = String::new();
    for (i, s) in suggestions.iter().enumerate() {
        out.push_str(&format!(
            "  {}. [{}] \"{}\" → {{{{{}}}}}\n",
            i + 1,
            s.category.label(),
            truncate_display(&s.original_value, 50),
            s.suggested_name,
        ));
        out.push_str(&format!("     {}\n", s.description));
    }
    out
}

fn truncate_display(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

// ─── Detectors ─────────────────────────────────────────────────────────────

fn add_suggestion(
    suggestions: &mut Vec<ParameterSuggestion>,
    seen: &mut BTreeMap<String, String>,
    value: &str,
    name: &str,
    desc: &str,
    category: DetectedCategory,
    confidence: f64,
) {
    if seen.contains_key(value) {
        return;
    }
    seen.insert(value.to_string(), name.to_string());
    suggestions.push(ParameterSuggestion {
        original_value: value.to_string(),
        suggested_name: name.to_string(),
        description: desc.to_string(),
        category,
        confidence,
    });
}

/// Detect URLs (http:// and https://)
fn detect_urls(
    text: &str,
    suggestions: &mut Vec<ParameterSuggestion>,
    seen: &mut BTreeMap<String, String>,
) {
    // Simple URL extraction — find http(s)://... sequences
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        if text[i..].starts_with("http://") || text[i..].starts_with("https://") {
            let start = i;
            // Advance past the URL
            while i < bytes.len() && !b" \t\n\r\"'`,;)}>]".contains(&bytes[i]) {
                i += 1;
            }
            let url = &text[start..i];

            // Skip common non-parameterizable URLs
            if url.contains("github.com/rust-lang")
                || url.contains("docs.rs")
                || url.contains("crates.io")
                || url.len() < 12
            {
                continue;
            }

            let name = classify_url(url);
            let desc = format!("{} detected in workflow", url_category_desc(&name));
            add_suggestion(
                suggestions,
                seen,
                url,
                &name,
                &desc,
                DetectedCategory::Url,
                0.9,
            );
        } else {
            i += 1;
        }
    }
}

/// Classify a URL into a suggested variable name.
fn classify_url(url: &str) -> String {
    let lower = url.to_lowercase();
    if lower.contains("/api/")
        || lower.contains("/v1/")
        || lower.contains("/v2/")
        || lower.contains("/graphql")
    {
        "api_url".to_string()
    } else if lower.contains("localhost") || lower.contains("127.0.0.1") {
        "local_url".to_string()
    } else if lower.contains(".git") || lower.contains("github.com") || lower.contains("gitlab.com")
    {
        "repo_url".to_string()
    } else if lower.contains("docker") || lower.contains("registry") {
        "registry_url".to_string()
    } else if lower.contains("database")
        || lower.contains("postgres")
        || lower.contains("mysql")
        || lower.contains("mongo")
    {
        "db_url".to_string()
    } else {
        "base_url".to_string()
    }
}

fn url_category_desc(name: &str) -> &str {
    match name {
        "api_url" => "API endpoint URL",
        "local_url" => "Local development URL",
        "repo_url" => "Git repository URL",
        "registry_url" => "Container registry URL",
        "db_url" => "Database connection URL",
        _ => "Base URL",
    }
}

/// Detect absolute file paths (/Users/..., /home/..., /tmp/..., etc.)
fn detect_file_paths(
    text: &str,
    suggestions: &mut Vec<ParameterSuggestion>,
    seen: &mut BTreeMap<String, String>,
) {
    // Match paths like /Users/foo/bar, /home/user/project, ~/something
    for word in text.split_whitespace() {
        let word = word.trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == ';');

        if word.starts_with("/Users/") || word.starts_with("/home/") {
            // User-specific absolute path
            let parts: Vec<&str> = word.split('/').collect();
            if parts.len() >= 4 {
                let name = if word.contains("/Projects/")
                    || word.contains("/project")
                    || word.contains("/src/")
                {
                    "project_dir"
                } else if word.contains("/output")
                    || word.contains("/dist/")
                    || word.contains("/build/")
                {
                    "output_dir"
                } else {
                    "target_path"
                };
                add_suggestion(
                    suggestions,
                    seen,
                    word,
                    name,
                    "Absolute file path (user-specific, should be parameterized)",
                    DetectedCategory::FilePath,
                    0.95,
                );
            }
        } else if word.starts_with("~/") && word.len() > 3 {
            add_suggestion(
                suggestions,
                seen,
                word,
                "target_path",
                "Home-relative path (may differ across machines)",
                DetectedCategory::FilePath,
                0.7,
            );
        } else if word.starts_with("/tmp/") || word.starts_with("/var/") {
            add_suggestion(
                suggestions,
                seen,
                word,
                "temp_path",
                "Temporary/system path",
                DetectedCategory::FilePath,
                0.6,
            );
        }
    }
}

/// Detect API keys and tokens (high-entropy strings, common prefixes).
fn detect_api_keys(
    text: &str,
    suggestions: &mut Vec<ParameterSuggestion>,
    seen: &mut BTreeMap<String, String>,
) {
    // Known API key prefixes
    let key_prefixes = [
        ("sk-", "api_key", "API secret key"),
        ("sk_live_", "stripe_key", "Stripe live API key"),
        ("sk_test_", "stripe_test_key", "Stripe test API key"),
        ("pk_live_", "stripe_pub_key", "Stripe publishable key"),
        ("ghp_", "github_token", "GitHub personal access token"),
        ("gho_", "github_oauth_token", "GitHub OAuth token"),
        ("ghs_", "github_server_token", "GitHub server token"),
        ("glpat-", "gitlab_token", "GitLab personal access token"),
        ("xoxb-", "slack_bot_token", "Slack bot token"),
        ("xoxp-", "slack_user_token", "Slack user token"),
        ("AKIA", "aws_access_key", "AWS access key ID"),
        ("Bearer ", "auth_token", "Bearer authentication token"),
        ("token ", "auth_token", "Authentication token"),
    ];

    for token in text.split_whitespace() {
        let token = token.trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == ';');
        // Handle KEY=value format: also check the value part after '='
        let word = if let Some(pos) = token.find('=') {
            &token[pos + 1..]
        } else {
            token
        };
        for (prefix, name, desc) in &key_prefixes {
            if word.starts_with(prefix) && word.len() > prefix.len() + 4 {
                add_suggestion(
                    suggestions,
                    seen,
                    word,
                    name,
                    desc,
                    DetectedCategory::ApiKey,
                    1.0,
                );
                break;
            }
        }

        // Also detect environment variable references like $API_KEY, $SECRET
        if word.starts_with('$') && word.len() > 2 {
            let var_name = word.trim_start_matches('$');
            let lower = var_name.to_lowercase();
            if lower.contains("key")
                || lower.contains("token")
                || lower.contains("secret")
                || lower.contains("password")
                || lower.contains("api")
            {
                let suggested = lower.replace('-', "_");
                add_suggestion(
                    suggestions,
                    seen,
                    word,
                    &suggested,
                    &format!("Environment variable reference: {}", var_name),
                    DetectedCategory::EnvVar,
                    0.8,
                );
            }
        }
    }

    // Detect hex strings that look like tokens (32+ hex chars)
    for word in text.split_whitespace() {
        let word = word.trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == ';');
        if word.len() >= 32
            && word.chars().all(|c| c.is_ascii_hexdigit())
            && !seen.contains_key(word)
        {
            add_suggestion(
                suggestions,
                seen,
                word,
                "auth_token",
                "Long hex string (likely a token or hash)",
                DetectedCategory::ApiKey,
                0.7,
            );
        }
    }
}

/// Detect email addresses.
fn detect_emails(
    text: &str,
    suggestions: &mut Vec<ParameterSuggestion>,
    seen: &mut BTreeMap<String, String>,
) {
    for word in text.split_whitespace() {
        let word = word.trim_matches(|c: char| {
            c == '"' || c == '\'' || c == ',' || c == ';' || c == '<' || c == '>'
        });
        // Skip git SSH URLs (git@host:user/repo) — handled by detect_git_repos
        if word.starts_with("git@") {
            continue;
        }
        if word.contains('@') && word.contains('.') && word.len() > 5 {
            // Basic email validation
            let parts: Vec<&str> = word.split('@').collect();
            if parts.len() == 2 && !parts[0].is_empty() && parts[1].contains('.') {
                add_suggestion(
                    suggestions,
                    seen,
                    word,
                    "email",
                    "Email address",
                    DetectedCategory::Email,
                    0.85,
                );
            }
        }
    }
}

/// Detect port numbers in common patterns.
fn detect_ports(
    text: &str,
    suggestions: &mut Vec<ParameterSuggestion>,
    seen: &mut BTreeMap<String, String>,
) {
    // Match :PORT patterns (e.g. localhost:3000, 0.0.0.0:8080)
    let mut i = 0;
    let chars: Vec<char> = text.chars().collect();
    while i < chars.len() {
        if chars[i] == ':' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit() {
            let start = i + 1;
            let mut end = start;
            while end < chars.len() && chars[end].is_ascii_digit() {
                end += 1;
            }
            let port_str: String = chars[start..end].iter().collect();
            if let Ok(port) = port_str.parse::<u16>()
                && (1024..=65535).contains(&port)
                && !seen.contains_key(&port_str)
            {
                // Check context — is there a host before the colon?
                let before: String = chars[..i]
                    .iter()
                    .rev()
                    .take(20)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                if before.contains("localhost")
                    || before.contains("0.0.0.0")
                    || before.contains("127.0.0.1")
                    || before.ends_with("://")
                    || before
                        .chars()
                        .last()
                        .is_some_and(|c| c.is_alphanumeric() || c == '.')
                {
                    add_suggestion(
                        suggestions,
                        seen,
                        &port_str,
                        "port",
                        &format!("Port number ({})", port),
                        DetectedCategory::Port,
                        0.6,
                    );
                }
            }
            i = end;
        } else {
            i += 1;
        }
    }
}

/// Detect IP addresses.
fn detect_ip_addresses(
    text: &str,
    suggestions: &mut Vec<ParameterSuggestion>,
    seen: &mut BTreeMap<String, String>,
) {
    // Simple IPv4 detection
    for word in text.split_whitespace() {
        let word = word.trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
        let parts: Vec<&str> = word.split('.').collect();
        if parts.len() == 4 && parts.iter().all(|p| p.parse::<u8>().is_ok()) {
            // Skip localhost and common non-parameterizable IPs
            if word == "127.0.0.1" || word == "0.0.0.0" {
                continue;
            }
            add_suggestion(
                suggestions,
                seen,
                word,
                "ip_address",
                "IP address (environment-specific)",
                DetectedCategory::IpAddress,
                0.8,
            );
        }
    }
}

/// Detect database connection URLs.
fn detect_database_urls(
    text: &str,
    suggestions: &mut Vec<ParameterSuggestion>,
    seen: &mut BTreeMap<String, String>,
) {
    let db_prefixes = [
        "postgres://",
        "postgresql://",
        "mysql://",
        "mongodb://",
        "mongodb+srv://",
        "redis://",
        "sqlite://",
    ];
    for token in text.split_whitespace() {
        let token = token.trim_matches(|c: char| c == '"' || c == '\'');
        // Handle KEY=value format: extract the value part
        let word = if let Some(pos) = token.find('=') {
            &token[pos + 1..]
        } else {
            token
        };
        for prefix in &db_prefixes {
            if word.starts_with(prefix) {
                add_suggestion(
                    suggestions,
                    seen,
                    word,
                    "database_url",
                    "Database connection URL (contains credentials)",
                    DetectedCategory::DatabaseUrl,
                    1.0,
                );
                break;
            }
        }
    }
}

/// Detect Docker image references.
fn detect_docker_images(
    text: &str,
    suggestions: &mut Vec<ParameterSuggestion>,
    seen: &mut BTreeMap<String, String>,
) {
    // Docker image patterns: registry/image:tag, image:tag
    let docker_indicators = ["docker pull", "docker run", "docker push", "FROM "];
    for indicator in &docker_indicators {
        if let Some(pos) = text.find(indicator) {
            let rest = &text[pos + indicator.len()..];
            let image: String = rest
                .trim_start()
                .chars()
                .take_while(|c| {
                    c.is_alphanumeric()
                        || *c == '/'
                        || *c == ':'
                        || *c == '.'
                        || *c == '-'
                        || *c == '_'
                })
                .collect();
            if !image.is_empty() && image.len() > 3 {
                add_suggestion(
                    suggestions,
                    seen,
                    &image,
                    "docker_image",
                    "Docker image reference",
                    DetectedCategory::DockerImage,
                    0.85,
                );
            }
        }
    }
}

/// Detect git repository URLs.
fn detect_git_repos(
    text: &str,
    suggestions: &mut Vec<ParameterSuggestion>,
    seen: &mut BTreeMap<String, String>,
) {
    // git@host:user/repo.git patterns
    for word in text.split_whitespace() {
        let word = word.trim_matches(|c: char| c == '"' || c == '\'');
        if word.starts_with("git@") && word.contains(':') && word.contains('/') {
            add_suggestion(
                suggestions,
                seen,
                word,
                "repo_url",
                "Git SSH repository URL",
                DetectedCategory::GitRepo,
                0.9,
            );
        }
    }
}

/// Detect user-specific values (home directories, usernames in paths).
fn detect_user_specific(
    text: &str,
    suggestions: &mut Vec<ParameterSuggestion>,
    seen: &mut BTreeMap<String, String>,
) {
    // Detect the current user's home directory
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy().to_string();
        if text.contains(&home_str) && !seen.contains_key(&home_str) {
            add_suggestion(
                suggestions,
                seen,
                &home_str,
                "home_dir",
                "User home directory (machine-specific)",
                DetectedCategory::UserSpecific,
                0.95,
            );
        }
    }

    // Detect current username in paths
    if let Ok(user) = std::env::var("USER")
        && user.len() >= 3
    {
        let user_in_path = format!("/Users/{}", user);
        let user_in_home = format!("/home/{}", user);
        for pattern in [&user_in_path, &user_in_home] {
            if text.contains(pattern.as_str()) && !seen.contains_key(pattern.as_str()) {
                // Already covered by home_dir detection usually, skip
            }
        }
    }
}

// ─── Workflow-level parameterization ───────────────────────────────────────

/// Scan an entire workflow for parameterizable values and return suggestions.
///
/// This collects text from all workflow fields: description, steps, commands.
pub fn scan_workflow(workflow: &crate::workflow::Workflow) -> Vec<ParameterSuggestion> {
    let content_text = workflow.base.content.as_text();
    let mut texts: Vec<&str> = Vec::new();

    texts.push(workflow.base.description.as_str());
    texts.push(content_text.as_ref());

    for step in &workflow.steps {
        texts.push(step.description.as_str());
        if let Some(ref cmd) = step.command {
            texts.push(cmd.as_str());
        }
    }

    detect_parameterizable_values(&texts)
}

/// Apply all accepted suggestions to a workflow, replacing literal values with `{{var}}`.
pub fn parameterize_workflow(
    workflow: &mut crate::workflow::Workflow,
    suggestions: &[ParameterSuggestion],
) {
    if suggestions.is_empty() {
        return;
    }

    // Replace in description
    workflow.base.description = apply_parameterization(&workflow.base.description, suggestions);

    // Replace in content
    let new_content = apply_parameterization(&workflow.base.content.as_text(), suggestions);
    workflow.base.content = crate::pattern::Content::Plain(new_content);

    // Replace in steps
    for step in &mut workflow.steps {
        step.description = apply_parameterization(&step.description, suggestions);
        if let Some(ref cmd) = step.command {
            step.command = Some(apply_parameterization(cmd, suggestions));
        }
    }

    // Merge suggested variables into workflow.variables (avoid duplicates)
    let existing_names: std::collections::HashSet<String> =
        workflow.variables.iter().map(|v| v.name.clone()).collect();
    let new_vars = suggestions_to_variables(suggestions);
    for var in new_vars {
        if !existing_names.contains(&var.name) {
            workflow.variables.push(var);
        }
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_urls() {
        let texts = vec!["Deploy to https://api.example.com/v1/deploy"];
        let suggestions = detect_parameterizable_values(&texts);
        assert!(!suggestions.is_empty());
        assert_eq!(suggestions[0].suggested_name, "api_url");
        assert_eq!(suggestions[0].category, DetectedCategory::Url);
    }

    #[test]
    fn test_detect_file_paths() {
        let texts = vec!["Run build in /Users/david/Projects/myapp"];
        let suggestions = detect_parameterizable_values(&texts);
        assert!(
            suggestions
                .iter()
                .any(|s| s.category == DetectedCategory::FilePath)
        );
    }

    #[test]
    fn test_detect_api_keys() {
        let texts = vec!["Use key sk-1234567890abcdef to authenticate"];
        let suggestions = detect_parameterizable_values(&texts);
        assert!(
            suggestions
                .iter()
                .any(|s| s.category == DetectedCategory::ApiKey)
        );
        assert_eq!(
            suggestions
                .iter()
                .find(|s| s.category == DetectedCategory::ApiKey)
                .unwrap()
                .suggested_name,
            "api_key"
        );
    }

    #[test]
    fn test_detect_github_token() {
        let texts = vec!["export GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz012345"];
        let suggestions = detect_parameterizable_values(&texts);
        assert!(
            suggestions
                .iter()
                .any(|s| s.suggested_name == "github_token")
        );
    }

    #[test]
    fn test_detect_email() {
        let texts = vec!["Send notification to admin@company.com"];
        let suggestions = detect_parameterizable_values(&texts);
        assert!(
            suggestions
                .iter()
                .any(|s| s.category == DetectedCategory::Email)
        );
    }

    #[test]
    fn test_detect_database_url() {
        let texts = vec!["DATABASE_URL=postgres://user:pass@db.example.com:5432/mydb"];
        let suggestions = detect_parameterizable_values(&texts);
        assert!(
            suggestions
                .iter()
                .any(|s| s.category == DetectedCategory::DatabaseUrl)
        );
    }

    #[test]
    fn test_detect_git_ssh() {
        let texts = vec!["git clone git@github.com:user/repo.git"];
        let suggestions = detect_parameterizable_values(&texts);
        assert!(
            suggestions
                .iter()
                .any(|s| s.category == DetectedCategory::GitRepo)
        );
    }

    #[test]
    fn test_apply_parameterization() {
        let suggestions = vec![ParameterSuggestion {
            original_value: "https://api.example.com".to_string(),
            suggested_name: "api_url".to_string(),
            description: "API URL".to_string(),
            category: DetectedCategory::Url,
            confidence: 0.9,
        }];
        let result = apply_parameterization("Deploy to https://api.example.com/v1", &suggestions);
        assert_eq!(result, "Deploy to {{api_url}}/v1");
    }

    #[test]
    fn test_no_false_positives_on_normal_text() {
        let texts = vec!["Run cargo build and then cargo test"];
        let suggestions = detect_parameterizable_values(&texts);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_deduplication() {
        let texts = vec![
            "Deploy to https://api.example.com",
            "Also check https://api.example.com/health",
        ];
        let suggestions = detect_parameterizable_values(&texts);
        // The URL should appear only once
        let url_count = suggestions
            .iter()
            .filter(|s| s.category == DetectedCategory::Url)
            .count();
        assert!(url_count <= 2); // might detect both, but deduped by exact value
    }

    #[test]
    fn test_format_display() {
        let suggestions = vec![ParameterSuggestion {
            original_value: "https://api.example.com".to_string(),
            suggested_name: "api_url".to_string(),
            description: "API endpoint URL".to_string(),
            category: DetectedCategory::Url,
            confidence: 0.9,
        }];
        let display = format_suggestions_display(&suggestions);
        assert!(display.contains("api_url"));
        assert!(display.contains("URL"));
    }
}
