//! Auto-detect project type and generate starter patterns from a built-in knowledge base.
//!
//! On first `mur context` in a new project directory, this module detects the project
//! language, extracts dependencies, and generates relevant starter patterns.

use anyhow::{Context, Result};
use chrono::Utc;
use mur_common::knowledge::{KnowledgeBase, Maturity};
use mur_common::pattern::{
    Applies, Content, Origin, OriginTrigger, Pattern, PatternKind, Tags, Tier,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

// ─── Project Detection ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    Swift,
    JavaScript,
    TypeScript,
    Python,
    Go,
    Php,
    Ruby,
}

impl Language {
    pub fn as_str(&self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Swift => "swift",
            Language::JavaScript => "javascript",
            Language::TypeScript => "typescript",
            Language::Python => "python",
            Language::Go => "go",
            Language::Php => "php",
            Language::Ruby => "ruby",
        }
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Detect the primary project language from files in the given directory.
pub fn detect_language(dir: &Path) -> Option<Language> {
    // Order matters: more specific first
    if dir.join("Cargo.toml").exists() {
        Some(Language::Rust)
    } else if dir.join("Package.swift").exists() {
        Some(Language::Swift)
    } else if dir.join("go.mod").exists() {
        Some(Language::Go)
    } else if dir.join("composer.json").exists() {
        Some(Language::Php)
    } else if dir.join("Gemfile").exists() {
        Some(Language::Ruby)
    } else if dir.join("pyproject.toml").exists() || dir.join("requirements.txt").exists() {
        Some(Language::Python)
    } else if dir.join("package.json").exists() {
        // Check for TypeScript
        let is_ts = dir.join("tsconfig.json").exists();
        if is_ts {
            Some(Language::TypeScript)
        } else {
            Some(Language::JavaScript)
        }
    } else {
        None
    }
}

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

fn extract_cargo_deps(dir: &Path) -> Vec<String> {
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
        if in_deps {
            if let Some(name) = trimmed.split('=').next() {
                let name = name.trim();
                if !name.is_empty() && !name.starts_with('#') {
                    deps.push(name.to_string());
                }
            }
        }
    }
    deps
}

fn extract_swift_deps(dir: &Path) -> Vec<String> {
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

fn extract_npm_deps(dir: &Path) -> Vec<String> {
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

fn extract_python_deps(dir: &Path) -> Vec<String> {
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
    let trimmed = line.trim().trim_matches('"').trim_matches('\'').trim_matches(',');
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

fn extract_go_deps(dir: &Path) -> Vec<String> {
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
            if let Some(path) = rest.split_whitespace().next() {
                if let Some(name) = path.rsplit('/').next() {
                    deps.push(name.to_string());
                }
            }
        }
    }
    deps
}

fn extract_composer_deps(dir: &Path) -> Vec<String> {
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

fn extract_gemfile_deps(dir: &Path) -> Vec<String> {
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

// ─── Built-in Knowledge Base ────────────────────────────────────────

struct StarterTemplate {
    lang: Language,
    dep: &'static str,
    name: &'static str,
    description: &'static str,
    content: &'static str,
    kind: PatternKind,
}

const TEMPLATES: &[StarterTemplate] = &[
    // ── Rust ──
    StarterTemplate {
        lang: Language::Rust,
        dep: "tokio",
        name: "rust-async-runtime-tokio",
        description: "Tokio async runtime patterns for this Rust project",
        content: "This project uses Tokio for async. Use #[tokio::main] for the entry point and #[tokio::test] for async tests. Prefer tokio::spawn for concurrent tasks, and use tokio::select! to race multiple futures. Avoid blocking the runtime with std::thread::sleep — use tokio::time::sleep instead.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::Rust,
        dep: "anyhow",
        name: "rust-error-handling-anyhow",
        description: "Error handling with anyhow context chains",
        content: "This project uses anyhow for error handling. Always attach context with .with_context(|| format!(\"...\")) instead of bare .unwrap() or ?. Use anyhow::bail! for early returns and anyhow::ensure! for preconditions. Reserve thiserror for public API boundaries; anyhow is for application-level errors.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::Rust,
        dep: "thiserror",
        name: "rust-custom-errors-thiserror",
        description: "Custom error types with thiserror",
        content: "This project uses thiserror for typed errors. Define error enums with #[derive(thiserror::Error)] and #[error(\"...\")] display messages. Use #[from] for automatic From implementations. Keep error variants specific — don't use a catch-all Other(String) variant when you can name the failure mode.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::Rust,
        dep: "serde",
        name: "rust-serialization-serde",
        description: "Serde serialization patterns",
        content: "This project uses Serde for serialization. Derive both Serialize and Deserialize on data types. Use #[serde(rename_all = \"snake_case\")] for consistent field naming, #[serde(default)] for optional fields, and #[serde(skip_serializing_if = \"Option::is_none\")] to keep output clean. Use #[serde(flatten)] to embed structs without nesting.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::Rust,
        dep: "axum",
        name: "rust-web-axum",
        description: "Axum web handler patterns",
        content: "This project uses Axum for HTTP. Define handlers as async functions that return impl IntoResponse. Use axum::extract::{State, Path, Query, Json} for typed extraction. Share state via Arc<AppState> with .with_state(). Group related routes with Router::nest() and apply middleware with .layer().",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::Rust,
        dep: "clap",
        name: "rust-cli-clap",
        description: "Clap CLI argument parsing",
        content: "This project uses Clap for CLI parsing. Prefer the derive API: #[derive(clap::Parser)] on a struct with #[arg(...)] attributes. Use subcommands via #[derive(clap::Subcommand)] enum. Set default values with #[arg(default_value = \"...\")]. Use #[arg(value_enum)] for enum arguments.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::Rust,
        dep: "tracing",
        name: "rust-logging-tracing",
        description: "Structured logging with tracing",
        content: "This project uses tracing for structured logging. Use tracing::{info, debug, warn, error} macros with key=value fields: info!(user=%name, action=\"login\"). Add #[tracing::instrument] to functions for automatic span creation. Initialize with tracing_subscriber::fmt().with_env_filter(\"RUST_LOG\").init().",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::Rust,
        dep: "sqlx",
        name: "rust-database-sqlx",
        description: "SQLx async database patterns",
        content: "This project uses SQLx for async database access. Use sqlx::query!() or sqlx::query_as!() macros for compile-time checked SQL. Run migrations with sqlx::migrate!(). Use connection pools (PgPool/SqlitePool) and pass them via shared state. Prefer transactions with pool.begin() for multi-step operations.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::Rust,
        dep: "reqwest",
        name: "rust-http-client-reqwest",
        description: "HTTP client patterns with reqwest",
        content: "This project uses reqwest for HTTP requests. Reuse a reqwest::Client instance (it pools connections internally). Use .json(&body) for POST bodies and .json::<T>() to deserialize responses. Always check .error_for_status()? to convert HTTP errors into Results. Set timeouts with ClientBuilder::timeout().",
        kind: PatternKind::Technical,
    },

    // ── Swift ──
    StarterTemplate {
        lang: Language::Swift,
        dep: "SwiftUI",
        name: "swift-swiftui-composition",
        description: "SwiftUI view composition and state management",
        content: "This project uses SwiftUI. Prefer small, composable views — extract subviews when a body exceeds ~30 lines. Use @State for view-local state, @Binding to pass mutable state down, and @Observable classes for shared state. Avoid @EnvironmentObject for anything that needs explicit dependency tracking.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::Swift,
        dep: "swift-testing",
        name: "swift-testing-framework",
        description: "Swift Testing framework patterns",
        content: "This project uses Swift Testing. Use @Test for test functions and #expect() for assertions instead of XCTAssert. Group related tests with @Suite. Use @Test(arguments:) for parameterized tests. Async tests work naturally — just mark the function async. Use #require() when a nil value should fail the test.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::Swift,
        dep: "Combine",
        name: "swift-combine-reactive",
        description: "Combine publisher/subscriber patterns",
        content: "This project uses Combine. Use AnyPublisher for API return types, and sink/store(in:) for subscriptions. Chain operators like map, flatMap, and catch for transformations. Use @Published properties in ObservableObject classes. Prefer Combine over completion handlers for async chains, but consider async/await for new code.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::Swift,
        dep: "SwiftData",
        name: "swift-swiftdata-persistence",
        description: "SwiftData model and persistence patterns",
        content: "This project uses SwiftData. Define models with @Model macro and use modelContainer modifier on the root view. Query data with @Query property wrapper. Use modelContext.insert() and modelContext.delete() for mutations. Define relationships with arrays of other @Model types. Use #Predicate for type-safe filtering.",
        kind: PatternKind::Technical,
    },

    // ── JavaScript/TypeScript ──
    StarterTemplate {
        lang: Language::JavaScript,
        dep: "react",
        name: "js-react-component-patterns",
        description: "React component and hooks patterns",
        content: "This project uses React. Prefer function components with hooks over class components. Use useState for local state, useEffect for side effects (always specify deps), and useCallback/useMemo only when you have measured a performance problem. Lift state up to the nearest common ancestor, not into global state.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::TypeScript,
        dep: "react",
        name: "ts-react-component-patterns",
        description: "React component and hooks patterns",
        content: "This project uses React with TypeScript. Prefer function components with hooks. Type props with interfaces, not type aliases. Use React.FC sparingly — prefer explicit return types. Use useState<T> for typed state, and define event handler types explicitly (e.g., React.MouseEvent<HTMLButtonElement>).",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::JavaScript,
        dep: "next",
        name: "js-nextjs-app-router",
        description: "Next.js App Router patterns",
        content: "This project uses Next.js. Use the App Router (app/ directory) with server components by default — add 'use client' only when you need interactivity or hooks. Use loading.tsx for Suspense boundaries, error.tsx for error handling. Fetch data directly in server components, not in useEffect.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::TypeScript,
        dep: "next",
        name: "ts-nextjs-app-router",
        description: "Next.js App Router patterns",
        content: "This project uses Next.js with TypeScript. Use the App Router with server components by default — add 'use client' only for interactivity. Type page props with { params, searchParams } and use NextRequest/NextResponse for API routes. Fetch data in server components, not in useEffect.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::JavaScript,
        dep: "express",
        name: "js-express-middleware",
        description: "Express middleware and routing patterns",
        content: "This project uses Express. Use Router() for modular route groups. Apply middleware in order: parsing (express.json()), auth, then routes. Always call next() in middleware or send a response. Use async wrappers or express-async-errors to avoid unhandled promise rejections in async handlers.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::TypeScript,
        dep: "typescript",
        name: "ts-strict-mode-patterns",
        description: "TypeScript strict mode patterns",
        content: "This project uses TypeScript. Enable strict mode in tsconfig.json. Prefer unknown over any, and use type guards (is/in/instanceof) for narrowing. Use discriminated unions for state machines. Avoid type assertions (as) — if you need one, add a comment explaining why. Use satisfies for type checking without widening.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::JavaScript,
        dep: "vitest",
        name: "js-vitest-testing",
        description: "Vitest testing patterns",
        content: "This project uses Vitest for testing. Use describe/it/expect for structure. Use vi.fn() for mocks and vi.spyOn() for spying. Prefer toEqual for deep equality and toBe for primitives. Use beforeEach for test setup and afterEach for cleanup. Run tests in watch mode during development with vitest --watch.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::JavaScript,
        dep: "jest",
        name: "js-jest-testing",
        description: "Jest testing patterns",
        content: "This project uses Jest for testing. Use describe/it/expect blocks. Mock modules with jest.mock() and functions with jest.fn(). Use beforeEach/afterEach for setup and teardown. Use .resolves/.rejects for promise assertions. Keep tests focused — one logical assertion per test.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::JavaScript,
        dep: "tailwindcss",
        name: "js-tailwind-css",
        description: "Tailwind CSS utility-first patterns",
        content: "This project uses Tailwind CSS. Compose utilities directly in markup instead of writing custom CSS. Use responsive prefixes (sm:, md:, lg:) for breakpoints and state variants (hover:, focus:, dark:) for interactivity. Extract repeated utility combinations into components, not @apply rules. Configure theme extensions in tailwind.config.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::JavaScript,
        dep: "prisma",
        name: "js-prisma-orm",
        description: "Prisma schema-first ORM patterns",
        content: "This project uses Prisma. Define models in schema.prisma and run npx prisma generate after changes. Use prisma.model.findUnique/findMany for reads and create/update/upsert for writes. Use include/select for relation loading. Always handle unique constraint violations with try/catch on PrismaClientKnownRequestError.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::TypeScript,
        dep: "prisma",
        name: "ts-prisma-orm",
        description: "Prisma schema-first ORM patterns with TypeScript",
        content: "This project uses Prisma with TypeScript. The generated PrismaClient provides full type safety. Use Prisma.ModelGetPayload<> for typed query results with includes. Define models in schema.prisma and run npx prisma generate to update types. Use transactions with prisma.$transaction() for multi-step operations.",
        kind: PatternKind::Technical,
    },

    // ── Python ──
    StarterTemplate {
        lang: Language::Python,
        dep: "fastapi",
        name: "python-fastapi-endpoints",
        description: "FastAPI endpoint and Pydantic model patterns",
        content: "This project uses FastAPI. Define endpoints with @app.get/post decorators and type-annotated parameters for automatic validation. Use Pydantic BaseModel for request/response schemas. Use Depends() for dependency injection. Return typed response models, not raw dicts. Use async def for I/O-bound endpoints.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::Python,
        dep: "django",
        name: "python-django-mvt",
        description: "Django model-view-template patterns",
        content: "This project uses Django. Define models in models.py with proper field types and Meta classes. Use class-based views (ListView, DetailView) for CRUD. Run makemigrations after model changes. Use Django ORM querysets — chain filter/exclude/annotate instead of raw SQL. Keep business logic in models or services, not views.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::Python,
        dep: "pytest",
        name: "python-pytest-fixtures",
        description: "Pytest testing and fixture patterns",
        content: "This project uses pytest. Use @pytest.fixture for reusable test setup with appropriate scope (function/class/module/session). Use parametrize for data-driven tests. Prefer assert statements over unittest methods. Use tmp_path fixture for temporary files. Use conftest.py to share fixtures across test files.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::Python,
        dep: "pandas",
        name: "python-pandas-dataframe",
        description: "Pandas DataFrame operations",
        content: "This project uses Pandas. Use vectorized operations instead of iterating rows with iterrows(). Chain operations with .pipe() for readability. Use .loc[] for label-based and .iloc[] for position-based indexing. Prefer .assign() for adding columns in a chain. Use .groupby().agg() for aggregations.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::Python,
        dep: "sqlalchemy",
        name: "python-sqlalchemy-orm",
        description: "SQLAlchemy ORM patterns",
        content: "This project uses SQLAlchemy. Use the 2.0 style with mapped_column() and DeclarativeBase. Create sessions with Session(engine) and use context managers for automatic cleanup. Use select() statements with session.execute() instead of the legacy query() API. Define relationships with relationship() and back_populates.",
        kind: PatternKind::Technical,
    },

    // ── Go ──
    StarterTemplate {
        lang: Language::Go,
        dep: "gin",
        name: "go-gin-http-handlers",
        description: "Gin HTTP handler patterns",
        content: "This project uses Gin for HTTP. Define handlers as func(c *gin.Context). Use c.ShouldBindJSON(&req) for request parsing with validation tags. Group routes with r.Group() and apply middleware per group. Use c.JSON() for responses and c.AbortWithStatusJSON() for errors. Pass dependencies via closures, not globals.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::Go,
        dep: "cobra",
        name: "go-cobra-cli",
        description: "Cobra CLI command patterns",
        content: "This project uses Cobra for CLI. Define commands with &cobra.Command{} and register with parent.AddCommand(). Use PersistentFlags for inherited flags and Flags for command-specific. Use RunE (not Run) to return errors. Organize commands in a cmd/ package with one file per command. Use viper for config binding.",
        kind: PatternKind::Technical,
    },
    StarterTemplate {
        lang: Language::Go,
        dep: "gorm",
        name: "go-gorm-orm",
        description: "GORM ORM patterns",
        content: "This project uses GORM. Define models as structs with gorm.Model embedded for ID/timestamps. Use db.AutoMigrate() for schema sync. Chain scopes with .Where().Order().Limit() for queries. Use db.Transaction() for multi-step operations. Preload associations with .Preload() and define them with proper foreign key tags.",
        kind: PatternKind::Technical,
    },

    // ── PHP ──
    StarterTemplate {
        lang: Language::Php,
        dep: "laravel/framework",
        name: "php-laravel-patterns",
        description: "Laravel Eloquent and middleware patterns",
        content: "This project uses Laravel. Define Eloquent models with fillable/guarded properties and relationships (hasMany, belongsTo). Use form requests for validation. Apply middleware in routes or controllers. Use Artisan commands (make:model -mcr) to scaffold. Keep controllers thin — move business logic to service classes or actions.",
        kind: PatternKind::Technical,
    },
];

// ─── Project Tracking ───────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub struct ProjectInfo {
    pub path: String,
    pub language: Language,
    pub deps: Vec<String>,
    pub generated_at: String,
    pub patterns_generated: Vec<String>,
}

fn projects_dir() -> PathBuf {
    dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("projects")
}

fn project_hash(path: &Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let canonical = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    format!("{:012x}", hasher.finish())
}

/// Check if a project directory has already been processed.
/// O(1) — just a file existence check.
pub fn is_known_project(path: &Path) -> Result<bool> {
    let hash = project_hash(path);
    Ok(projects_dir().join(format!("{hash}.json")).exists())
}

/// Mark a project as known, recording what was generated.
pub fn mark_project_known(path: &Path, info: ProjectInfo) -> Result<()> {
    let dir = projects_dir();
    fs::create_dir_all(&dir).context("Failed to create projects dir")?;
    let hash = project_hash(path);
    let json = serde_json::to_string_pretty(&info)?;
    let file_path = dir.join(format!("{hash}.json"));
    fs::write(&file_path, json).context("Failed to write project info")?;
    Ok(())
}

// ─── Pattern Generation ─────────────────────────────────────────────

fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect::<String>()
        .replace("--", "-")
        .trim_matches('-')
        .to_string()
}

/// Generate starter patterns for a project directory.
/// Returns patterns for detected dependencies, skipping any whose name already exists.
pub fn generate_starter_patterns(
    cwd: &Path,
    existing_names: &HashSet<String>,
) -> Result<Vec<Pattern>> {
    let Some(lang) = detect_language(cwd) else {
        return Ok(vec![]);
    };
    let deps = extract_deps(cwd, lang);
    let dep_set: HashSet<&str> = deps.iter().map(|s| s.as_str()).collect();

    let now = Utc::now();
    let mut patterns = Vec::new();

    for t in TEMPLATES {
        // Match language (JS templates also apply to TS projects for shared deps)
        let lang_match = t.lang == lang
            || (lang == Language::TypeScript && t.lang == Language::JavaScript)
            || (lang == Language::JavaScript && t.lang == Language::TypeScript);
        if !lang_match {
            continue;
        }

        // Check if this dependency is in the project
        // For packages with slashes (laravel/framework), also check the full name
        let dep_found = dep_set.contains(t.dep)
            || dep_set.contains(&slugify(t.dep).as_str());
        if !dep_found {
            continue;
        }

        // Skip if pattern name already exists
        if existing_names.contains(t.name) {
            continue;
        }

        let pattern = Pattern {
            base: KnowledgeBase {
                name: t.name.to_string(),
                description: t.description.to_string(),
                content: Content::DualLayer {
                    technical: t.content.to_string(),
                    principle: None,
                },
                tier: Tier::Project,
                importance: 0.5,
                confidence: 0.5,
                tags: Tags {
                    languages: vec![lang.as_str().to_string()],
                    topics: vec!["starter".to_string(), slugify(t.dep)],
                    extra: Default::default(),
                },
                applies: Applies {
                    languages: vec![lang.as_str().to_string()],
                    ..Default::default()
                },
                maturity: Maturity::Draft,
                created_at: now,
                updated_at: now,
                ..Default::default()
            },
            kind: Some(t.kind),
            origin: Some(Origin {
                source: "starter".to_string(),
                trigger: OriginTrigger::Automatic,
                user: None,
                platform: None,
                confidence: 0.5,
            }),
            attachments: vec![],
        };
        patterns.push(pattern);
    }

    Ok(patterns)
}

/// Detect language and return its display name (for status messages).
pub fn detect_language_name(cwd: &Path) -> Option<String> {
    detect_language(cwd).map(|l| l.to_string())
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_detect_language_rust() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        assert_eq!(detect_language(dir.path()), Some(Language::Rust));
    }

    #[test]
    fn test_detect_language_swift() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Package.swift"), "// swift-tools-version:5.9").unwrap();
        assert_eq!(detect_language(dir.path()), Some(Language::Swift));
    }

    #[test]
    fn test_detect_language_js() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        assert_eq!(detect_language(dir.path()), Some(Language::JavaScript));
    }

    #[test]
    fn test_detect_language_ts() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
        assert_eq!(detect_language(dir.path()), Some(Language::TypeScript));
    }

    #[test]
    fn test_detect_language_python() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "[project]").unwrap();
        assert_eq!(detect_language(dir.path()), Some(Language::Python));
    }

    #[test]
    fn test_detect_language_go() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("go.mod"), "module example.com/test").unwrap();
        assert_eq!(detect_language(dir.path()), Some(Language::Go));
    }

    #[test]
    fn test_detect_language_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(detect_language(dir.path()), None);
    }

    #[test]
    fn test_extract_cargo_deps() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\n\n[dependencies]\ntokio = \"1\"\nserde = { version = \"1\", features = [\"derive\"] }\nanyhow = \"1\"\n\n[dev-dependencies]\ntempfile = \"3\"\n",
        ).unwrap();
        let deps = extract_cargo_deps(dir.path());
        assert!(deps.contains(&"tokio".to_string()));
        assert!(deps.contains(&"serde".to_string()));
        assert!(deps.contains(&"anyhow".to_string()));
        assert!(deps.contains(&"tempfile".to_string()));
    }

    #[test]
    fn test_extract_npm_deps() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"react":"^18","next":"^14"},"devDependencies":{"typescript":"^5","vitest":"^1"}}"#,
        ).unwrap();
        let deps = extract_npm_deps(dir.path());
        assert!(deps.contains(&"react".to_string()));
        assert!(deps.contains(&"next".to_string()));
        assert!(deps.contains(&"typescript".to_string()));
        assert!(deps.contains(&"vitest".to_string()));
    }

    #[test]
    fn test_extract_python_deps_requirements() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("requirements.txt"),
            "fastapi>=0.100\npandas==2.0\n# comment\npytest\n",
        ).unwrap();
        let deps = extract_python_deps(dir.path());
        assert!(deps.contains(&"fastapi".to_string()));
        assert!(deps.contains(&"pandas".to_string()));
        assert!(deps.contains(&"pytest".to_string()));
    }

    #[test]
    fn test_extract_go_deps() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "module example.com/test\n\ngo 1.21\n\nrequire (\n\tgithub.com/gin-gonic/gin v1.9.1\n\tgithub.com/spf13/cobra v1.7.0\n)\n",
        ).unwrap();
        let deps = extract_go_deps(dir.path());
        assert!(deps.contains(&"gin".to_string()));
        assert!(deps.contains(&"cobra".to_string()));
    }

    #[test]
    fn test_generate_patterns_for_rust_project() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\n\n[dependencies]\ntokio = \"1\"\nserde = \"1\"\n",
        ).unwrap();
        let existing = HashSet::new();
        let patterns = generate_starter_patterns(dir.path(), &existing).unwrap();
        assert!(!patterns.is_empty());
        let names: Vec<&str> = patterns.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"rust-async-runtime-tokio"));
        assert!(names.contains(&"rust-serialization-serde"));
        // Should NOT contain patterns for deps not in the project
        assert!(!names.contains(&"rust-web-axum"));
    }

    #[test]
    fn test_generate_patterns_skips_existing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\n\n[dependencies]\ntokio = \"1\"\n",
        ).unwrap();
        let mut existing = HashSet::new();
        existing.insert("rust-async-runtime-tokio".to_string());
        let patterns = generate_starter_patterns(dir.path(), &existing).unwrap();
        let names: Vec<&str> = patterns.iter().map(|p| p.name.as_str()).collect();
        assert!(!names.contains(&"rust-async-runtime-tokio"));
    }

    #[test]
    fn test_generate_patterns_unknown_deps() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\n\n[dependencies]\nsome-obscure-crate = \"1\"\n",
        ).unwrap();
        let existing = HashSet::new();
        let patterns = generate_starter_patterns(dir.path(), &existing).unwrap();
        assert!(patterns.is_empty());
    }

    #[test]
    fn test_generate_patterns_no_project() {
        let dir = tempfile::tempdir().unwrap();
        let existing = HashSet::new();
        let patterns = generate_starter_patterns(dir.path(), &existing).unwrap();
        assert!(patterns.is_empty());
    }

    #[test]
    fn test_project_tracking() {
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().join("myproject");
        fs::create_dir_all(&project_path).unwrap();

        // Not known yet
        assert!(!is_known_project(&project_path).unwrap());

        // Mark it
        let info = ProjectInfo {
            path: project_path.to_string_lossy().to_string(),
            language: Language::Rust,
            deps: vec!["tokio".to_string()],
            generated_at: "2026-03-06T00:00:00Z".to_string(),
            patterns_generated: vec!["rust-async-runtime-tokio".to_string()],
        };

        // Override projects dir for test
        let projects = projects_dir();
        fs::create_dir_all(&projects).ok();
        mark_project_known(&project_path, info).unwrap();
        assert!(is_known_project(&project_path).unwrap());
    }

    #[test]
    fn test_slugify() {
        assert_eq!(slugify("SwiftUI"), "swiftui");
        assert_eq!(slugify("laravel/framework"), "laravel-framework");
        assert_eq!(slugify("@scope/package"), "scope-package");
        assert_eq!(slugify("hello world"), "hello-world");
    }

    #[test]
    fn test_pattern_fields() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\n\n[dependencies]\ntokio = \"1\"\n",
        ).unwrap();
        let patterns = generate_starter_patterns(dir.path(), &HashSet::new()).unwrap();
        let p = &patterns[0];
        assert_eq!(p.tier, Tier::Project);
        assert_eq!(p.maturity, Maturity::Draft);
        assert!((p.confidence - 0.5).abs() < 0.001);
        assert!(p.tags.languages.contains(&"rust".to_string()));
        assert!(p.tags.topics.contains(&"starter".to_string()));
        assert_eq!(p.origin.as_ref().unwrap().source, "starter");
        assert_eq!(p.origin.as_ref().unwrap().trigger, OriginTrigger::Automatic);
    }

    #[test]
    fn test_extract_composer_deps() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("composer.json"),
            r#"{"require":{"php":"^8.1","laravel/framework":"^10"},"require-dev":{"phpunit/phpunit":"^10"}}"#,
        ).unwrap();
        let deps = extract_composer_deps(dir.path());
        assert!(deps.contains(&"laravel/framework".to_string()));
        assert!(deps.contains(&"phpunit/phpunit".to_string()));
        assert!(!deps.contains(&"php".to_string())); // php itself is excluded
    }

    #[test]
    fn test_extract_swift_deps() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Package.swift"),
            r#"
// swift-tools-version:5.9
import PackageDescription
let package = Package(
    name: "MyApp",
    dependencies: [
        .package(url: "https://github.com/apple/swift-testing.git", from: "0.1.0"),
    ],
    targets: [
        .target(name: "MyApp", dependencies: ["SwiftUI"]),
    ]
)
"#,
        ).unwrap();
        let deps = extract_swift_deps(dir.path());
        assert!(deps.contains(&"swift-testing".to_string()));
        assert!(deps.contains(&"SwiftUI".to_string()));
    }
}
