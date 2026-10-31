//! Dependency-name extraction from each language's project manifest.

use super::*;

/// Extract dependency names from the project's config files.
pub fn extract_deps(dir: &Path, lang: Language) -> Vec<String> {
    match lang {
        Language::Rust => extract_cargo_deps(dir),
        Language::Swift => extract_swift_deps(dir),
        Language::JavaScript | Language::TypeScript => extract_npm_deps(dir),
        Language::Python => extract_python_deps(dir),
        Language::Go => extract_go_deps(dir),
        Language::Php => extract_composer_deps(dir),
        Language::Ruby => extract_gemfile_deps(dir),
    }
}

pub(super) fn extract_cargo_deps(dir: &Path) -> Vec<String> {
    let Ok(content) = fs::read_to_string(dir.join("Cargo.toml")) else {
        return vec![];
    };
    let mut deps = Vec::new();
    let mut in_deps = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_deps = trimmed == "[dependencies]"
                || trimmed.starts_with("[dependencies.")
                || trimmed == "[dev-dependencies]"
                || trimmed.starts_with("[dev-dependencies.");
            continue;
        }
        if in_deps && let Some(name) = trimmed.split('=').next() {
            let name = name.trim();
            if !name.is_empty() && !name.starts_with('#') {
                deps.push(name.to_string());
            }
        }
    }
    deps
}

pub(super) fn extract_swift_deps(dir: &Path) -> Vec<String> {
    let Ok(content) = fs::read_to_string(dir.join("Package.swift")) else {
        return vec![];
    };
    let mut deps = Vec::new();
    let re = regex::Regex::new(r#"\.package\([^)]*url:\s*"[^"]*?/([^/"]+?)(?:\.git)?"#).ok();
    if let Some(re) = re {
        for cap in re.captures_iter(&content) {
            if let Some(name) = cap.get(1) {
                deps.push(name.as_str().to_string());
            }
        }
    }
    // Detect SwiftUI / Swift Testing from target dependencies or imports
    if content.contains("SwiftUI") || content.contains("swiftui") {
        deps.push("SwiftUI".to_string());
    }
    if content.contains(".testing") || content.contains("swift-testing") {
        deps.push("swift-testing".to_string());
    }
    if content.contains("SwiftData") {
        deps.push("SwiftData".to_string());
    }
    if content.contains("Combine") {
        deps.push("Combine".to_string());
    }
    deps
}

pub(super) fn extract_npm_deps(dir: &Path) -> Vec<String> {
    let Ok(content) = fs::read_to_string(dir.join("package.json")) else {
        return vec![];
    };
    let Ok(json): Result<serde_json::Value, _> = serde_json::from_str(&content) else {
        return vec![];
    };
    let mut deps = Vec::new();
    for key in ["dependencies", "devDependencies"] {
        if let Some(obj) = json.get(key).and_then(|v| v.as_object()) {
            for name in obj.keys() {
                deps.push(name.clone());
            }
        }
    }
    deps
}

pub(super) fn extract_python_deps(dir: &Path) -> Vec<String> {
    let mut deps = Vec::new();

    // Try pyproject.toml
    if let Ok(content) = fs::read_to_string(dir.join("pyproject.toml")) {
        let mut in_deps = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed == "[project.dependencies]"
                || trimmed == "[tool.poetry.dependencies]"
                || trimmed.starts_with("dependencies = [")
            {
                in_deps = true;
                // Handle inline array start
                if trimmed.starts_with("dependencies = [") {
                    let inner = trimmed.trim_start_matches("dependencies = [");
                    parse_python_dep_items(inner, &mut deps);
                }
                continue;
            }
            if in_deps {
                if trimmed.starts_with('[') && !trimmed.starts_with("[\"") {
                    in_deps = false;
                    continue;
                }
                if trimmed == "]" {
                    in_deps = false;
                    continue;
                }
                parse_python_dep_items(trimmed, &mut deps);
            }
        }
    }

    // Try requirements.txt
    if let Ok(content) = fs::read_to_string(dir.join("requirements.txt")) {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
                continue;
            }
            // Strip version specifiers: package>=1.0 → package
            let name = trimmed
                .split(['>', '<', '=', '!', '~', ';', '['])
                .next()
                .unwrap_or("")
                .trim();
            if !name.is_empty() {
                deps.push(name.to_string());
            }
        }
    }

    deps
}

fn parse_python_dep_items(line: &str, deps: &mut Vec<String>) {
    // Handle quoted dependency strings like "fastapi>=0.100"
    let trimmed = line
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches(',');
    if trimmed.is_empty() || trimmed == "]" {
        return;
    }
    let name = trimmed
        .split(['>', '<', '=', '!', '~', ';', '['])
        .next()
        .unwrap_or("")
        .trim();
    if !name.is_empty() {
        deps.push(name.to_string());
    }
}

pub(super) fn extract_go_deps(dir: &Path) -> Vec<String> {
    let Ok(content) = fs::read_to_string(dir.join("go.mod")) else {
        return vec![];
    };
    let mut deps = Vec::new();
    let mut in_require = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("require (") || trimmed == "require (" {
            in_require = true;
            continue;
        }
        if trimmed == ")" {
            in_require = false;
            continue;
        }
        if in_require {
            // Lines like: github.com/gin-gonic/gin v1.9.1
            if let Some(path) = trimmed.split_whitespace().next() {
                // Extract last path segment as package name
                if let Some(name) = path.rsplit('/').next() {
                    deps.push(name.to_string());
                }
            }
        }
        // Single-line require
        if trimmed.starts_with("require ") && !trimmed.contains('(') {
            let rest = trimmed.trim_start_matches("require ");
            if let Some(path) = rest.split_whitespace().next()
                && let Some(name) = path.rsplit('/').next()
            {
                deps.push(name.to_string());
            }
        }
    }
    deps
}

pub(super) fn extract_composer_deps(dir: &Path) -> Vec<String> {
    let Ok(content) = fs::read_to_string(dir.join("composer.json")) else {
        return vec![];
    };
    let Ok(json): Result<serde_json::Value, _> = serde_json::from_str(&content) else {
        return vec![];
    };
    let mut deps = Vec::new();
    for key in ["require", "require-dev"] {
        if let Some(obj) = json.get(key).and_then(|v| v.as_object()) {
            for name in obj.keys() {
                if name != "php" {
                    deps.push(name.clone());
                }
            }
        }
    }
    deps
}

pub(super) fn extract_gemfile_deps(dir: &Path) -> Vec<String> {
    let Ok(content) = fs::read_to_string(dir.join("Gemfile")) else {
        return vec![];
    };
    let mut deps = Vec::new();
    let re = regex::Regex::new(r#"gem\s+['"]([^'"]+)['"]"#).ok();
    if let Some(re) = re {
        for cap in re.captures_iter(&content) {
            if let Some(name) = cap.get(1) {
                deps.push(name.as_str().to_string());
            }
        }
    }
    deps
}
