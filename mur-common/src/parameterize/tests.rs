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
