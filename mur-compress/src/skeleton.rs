//! Code skeletonization: parse a source file with tree-sitter and collapse
//! every function/method body to a short elision marker, keeping imports,
//! signatures, type/class declarations, and doc comments byte-for-byte.
//!
//! Used only for content that is already provably stale (see
//! `cc-proxy`'s supersession pass) — it never touches the most recent view
//! of a file, which is where an in-flight `Edit`'s `old_string` anchors. The
//! full original stays separately recoverable via the caller's own
//! retrieval hash; a skeleton is a denser *inline* summary, not the sole
//! copy of anything.
//!
//! Recognizes: Rust, Python, JavaScript, TypeScript (incl. TSX), Go, PHP,
//! Java, Swift, Dart. Other languages fall through to `None`; the caller
//! keeps its plain stub.
//!
//! Each grammar crate is pinned to whatever version emits the ABI the
//! workspace's shared `tree-sitter` core (`0.24`, set by `mur-core`'s
//! pre-existing Rust-only code-graph indexer) can load — newer grammar
//! releases target a newer core ABI and fail `Parser::set_language` at
//! runtime with no compile-time signal, so don't bump a grammar version
//! without re-running the language's skeletonize test.

use tree_sitter::{Language, Node, Parser};

struct LangSpec {
    language: fn() -> Language,
    /// Node kinds whose `body` field gets collapsed; recursion stops here.
    function_kinds: &'static [&'static str],
    /// Node kinds whose children are still worth walking into looking for
    /// nested function-like nodes (modules, classes, impl blocks, ...).
    container_kinds: &'static [&'static str],
}

fn lang_spec_for(ext: &str) -> Option<LangSpec> {
    Some(match ext {
        "rs" => LangSpec {
            language: || tree_sitter_rust::LANGUAGE.into(),
            function_kinds: &["function_item"],
            container_kinds: &[
                "source_file",
                "impl_item",
                "trait_item",
                "mod_item",
                "declaration_list",
            ],
        },
        "py" | "pyi" => LangSpec {
            language: || tree_sitter_python::LANGUAGE.into(),
            function_kinds: &["function_definition"],
            container_kinds: &["module", "class_definition", "block"],
        },
        "js" | "jsx" | "mjs" | "cjs" => LangSpec {
            language: || tree_sitter_javascript::LANGUAGE.into(),
            function_kinds: &[
                "function_declaration",
                "method_definition",
                "function_expression",
                "arrow_function",
            ],
            container_kinds: &[
                "program",
                "class_body",
                "class_declaration",
                "export_statement",
                "statement_block",
            ],
        },
        "ts" | "mts" | "cts" => LangSpec {
            language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            function_kinds: &[
                "function_declaration",
                "method_definition",
                "function_expression",
                "arrow_function",
            ],
            container_kinds: &[
                "program",
                "class_body",
                "class_declaration",
                "interface_declaration",
                "export_statement",
                "statement_block",
            ],
        },
        "tsx" => LangSpec {
            language: || tree_sitter_typescript::LANGUAGE_TSX.into(),
            function_kinds: &[
                "function_declaration",
                "method_definition",
                "function_expression",
                "arrow_function",
            ],
            container_kinds: &[
                "program",
                "class_body",
                "class_declaration",
                "interface_declaration",
                "export_statement",
                "statement_block",
            ],
        },
        "go" => LangSpec {
            language: || tree_sitter_go::LANGUAGE.into(),
            function_kinds: &["function_declaration", "method_declaration", "func_literal"],
            container_kinds: &["source_file"],
        },
        "php" | "phtml" => LangSpec {
            language: || tree_sitter_php::LANGUAGE_PHP.into(),
            function_kinds: &[
                "function_definition",
                "method_declaration",
                "anonymous_function",
                "arrow_function",
            ],
            container_kinds: &["program", "class_declaration", "declaration_list"],
        },
        "java" => LangSpec {
            language: || tree_sitter_java::LANGUAGE.into(),
            function_kinds: &["method_declaration", "constructor_declaration"],
            container_kinds: &[
                "program",
                "class_declaration",
                "class_body",
                "interface_body",
            ],
        },
        "swift" => LangSpec {
            language: || tree_sitter_swift::LANGUAGE.into(),
            function_kinds: &["function_declaration"],
            container_kinds: &["source_file", "class_declaration", "class_body"],
        },
        "dart" => LangSpec {
            language: tree_sitter_dart::language,
            function_kinds: &["lambda_expression"],
            container_kinds: &["program", "class_definition", "class_body"],
        },
        _ => return None,
    })
}

/// Recursively collect `(start_byte, end_byte)` ranges of every
/// function-like node's `body` field, in document order. Stops descending
/// once it enters a function-like node — nested closures/methods are
/// already inside the elided range.
fn collect_body_ranges(node: Node, spec: &LangSpec, out: &mut Vec<(usize, usize)>) {
    let kind = node.kind();
    if spec.function_kinds.contains(&kind) {
        if let Some(body) = node.child_by_field_name("body") {
            out.push((body.start_byte(), body.end_byte()));
        }
        return;
    }
    // Only descend through recognized containers (module/class/impl/...);
    // everything else (plain statements, expressions, decorators, ...) is
    // left as-is and not searched for nested function definitions. This
    // keeps arbitrary top-level code verbatim rather than risking a
    // misidentified elision inside it. The tree root is always one of
    // `container_kinds` for every language table above.
    if !spec.container_kinds.contains(&kind) {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_body_ranges(child, spec, out);
    }
}

/// Guess a tree-sitter language from a file path's extension. Returns the
/// bare extension (lowercased) for use as the cache/lookup key elsewhere.
pub fn language_hint(file_path: &str) -> Option<String> {
    let ext = file_path.rsplit('.').next()?.to_ascii_lowercase();
    lang_spec_for(&ext).map(|_| ext)
}

/// Skeletonize `source`, whose language is inferred from `file_path`'s
/// extension. Returns `None` if the extension is unrecognized, the source
/// fails to parse, or the result wouldn't actually be smaller than the
/// input (nothing to elide — e.g. no functions, or all one-liners).
pub fn skeletonize(source: &str, file_path: &str) -> Option<String> {
    let ext = file_path.rsplit('.').next()?.to_ascii_lowercase();
    let spec = lang_spec_for(&ext)?;
    let mut parser = Parser::new();
    parser.set_language(&(spec.language)()).ok()?;
    let tree = parser.parse(source, None)?;

    let mut ranges = Vec::new();
    collect_body_ranges(tree.root_node(), &spec, &mut ranges);
    if ranges.is_empty() {
        return None;
    }

    // Apply replacements back-to-front so earlier byte offsets stay valid.
    let mut out = source.to_string();
    for (start, end) in ranges.into_iter().rev() {
        if start >= end || end > out.len() {
            continue;
        }
        out.replace_range(start..end, "{ /* elided */ }");
    }
    (out.len() < source.len()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_hint_recognizes_supported_extensions() {
        assert_eq!(language_hint("src/main.rs").as_deref(), Some("rs"));
        assert_eq!(language_hint("app.py").as_deref(), Some("py"));
        assert_eq!(language_hint("index.tsx").as_deref(), Some("tsx"));
        assert_eq!(language_hint("README.md"), None);
    }

    #[test]
    fn skeletonizes_rust_function_bodies() {
        let src = "use std::fmt;\n\nfn add(a: i32, b: i32) -> i32 {\n    let sum = a + b;\n    sum\n}\n\nstruct Point { x: i32, y: i32 }\n";
        let out = skeletonize(src, "src/lib.rs").expect("should skeletonize");
        assert!(out.contains("use std::fmt;"));
        assert!(out.contains("fn add(a: i32, b: i32) -> i32"));
        assert!(out.contains("{ /* elided */ }"));
        assert!(!out.contains("let sum"));
        assert!(out.contains("struct Point { x: i32, y: i32 }"));
        assert!(out.len() < src.len());
    }

    #[test]
    fn skeletonizes_rust_impl_methods() {
        let src = "impl Point {\n    fn new() -> Self {\n        Self { x: 0, y: 0 }\n    }\n\n    fn dist(&self) -> f64 {\n        ((self.x * self.x + self.y * self.y) as f64).sqrt()\n    }\n}\n";
        let out = skeletonize(src, "src/lib.rs").expect("should skeletonize");
        assert!(out.contains("fn new() -> Self"));
        assert!(out.contains("fn dist(&self) -> f64"));
        assert!(!out.contains("sqrt"));
        assert_eq!(out.matches("{ /* elided */ }").count(), 2);
    }

    #[test]
    fn skeletonizes_python_functions_and_classes() {
        let src = "import os\n\n\ndef greet(name):\n    message = f\"hi {name}\"\n    return message\n\n\nclass Greeter:\n    def hello(self):\n        return greet(\"world\")\n";
        let out = skeletonize(src, "app.py").expect("should skeletonize");
        assert!(out.contains("import os"));
        assert!(out.contains("def greet(name):"));
        assert!(out.contains("class Greeter:"));
        assert!(out.contains("def hello(self):"));
        assert!(!out.contains("f\"hi {name}\""));
    }

    #[test]
    fn skeletonizes_javascript_functions() {
        let src = "import fs from 'fs';\n\nfunction add(a, b) {\n    const sum = a + b;\n    return sum;\n}\n\nclass Foo {\n    bar() {\n        return 1;\n    }\n}\n";
        let out = skeletonize(src, "src/foo.js").expect("should skeletonize");
        assert!(out.contains("import fs from 'fs';"));
        assert!(out.contains("function add(a, b)"));
        assert!(out.contains("bar()"));
        assert!(!out.contains("const sum"));
    }

    #[test]
    fn skeletonizes_typescript_with_types() {
        let src = "export function add(a: number, b: number): number {\n    const sum = a + b;\n    return sum;\n}\n";
        let out = skeletonize(src, "src/foo.ts").expect("should skeletonize");
        assert!(out.contains("function add(a: number, b: number): number"));
        assert!(!out.contains("const sum"));
    }

    #[test]
    fn skeletonizes_go_functions_and_methods() {
        let src = "package main\n\nfunc Add(a, b int) int {\n\tsum := a + b\n\treturn sum\n}\n\ntype T struct{ x int }\n\nfunc (t T) Double() int {\n\treturn t.x * 2\n}\n";
        let out = skeletonize(src, "main.go").expect("should skeletonize");
        assert!(out.contains("func Add(a, b int) int"));
        assert!(out.contains("func (t T) Double() int"));
        assert!(out.contains("type T struct{ x int }"));
        assert!(!out.contains("sum := a + b"));
    }

    #[test]
    fn skeletonizes_php_functions_and_methods() {
        let src = "<?php\n\nfunction add($a, $b) {\n    $sum = $a + $b;\n    return $sum;\n}\n\nclass Foo {\n    function bar() {\n        return 1;\n    }\n}\n";
        let out = skeletonize(src, "index.php").expect("should skeletonize");
        assert!(out.contains("function add($a, $b)"));
        assert!(out.contains("class Foo"));
        assert!(out.contains("function bar()"));
        assert!(!out.contains("$sum = $a + $b;"));
    }

    #[test]
    fn skeletonizes_java_methods() {
        let src = "public class Foo {\n    public int add(int a, int b) {\n        int sum = a + b;\n        return sum;\n    }\n}\n";
        let out = skeletonize(src, "Foo.java").expect("should skeletonize");
        assert!(out.contains("public int add(int a, int b)"));
        assert!(!out.contains("int sum = a + b;"));
    }

    #[test]
    fn skeletonizes_swift_functions() {
        let src = "func add(a: Int, b: Int) -> Int {\n    let sum = a + b\n    return sum\n}\n";
        let out = skeletonize(src, "Foo.swift").expect("should skeletonize");
        assert!(out.contains("func add(a: Int, b: Int) -> Int"));
        assert!(!out.contains("let sum = a + b"));
    }

    #[test]
    fn skeletonizes_dart_functions() {
        let src = "int add(int a, int b) {\n  int sum = a + b;\n  return sum;\n}\n";
        let out = skeletonize(src, "main.dart").expect("should skeletonize");
        assert!(out.contains("int add(int a, int b)"));
        assert!(!out.contains("int sum = a + b;"));
    }

    #[test]
    fn unsupported_extension_returns_none() {
        assert!(skeletonize("# just markdown\nsome text\n", "README.md").is_none());
    }

    #[test]
    fn no_functions_returns_none() {
        assert!(skeletonize("const X = 1;\nconst Y = 2;\n", "consts.js").is_none());
    }

    #[test]
    fn is_idempotent_shaped() {
        // Re-skeletonizing already-elided source finds no (further
        // shrinkable) function bodies of substance and should not blow up
        // or infinitely nest markers.
        let src = "fn f() {\n    let x = 1;\n    let y = 2;\n    x + y\n}\n";
        let once = skeletonize(src, "a.rs").expect("first pass elides the body");
        assert!(once.len() < src.len());
        let twice = skeletonize(&once, "a.rs");
        assert!(
            twice.is_none(),
            "already-elided body has nothing left to shrink"
        );
    }
}
