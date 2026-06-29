//! Tree-sitter-based extraction of top-level semantic units from Rust source.

use anyhow::{Result, anyhow};
use tree_sitter::{Node, Parser};

use super::{SemanticUnit, SupportedLanguage, UnitKind};

/// Parse `source` and return the top-level semantic units it contains.
pub fn extract_units(source: &[u8], lang: SupportedLanguage) -> Result<Vec<SemanticUnit>> {
    let mut parser = Parser::new();
    let ts_lang = match lang {
        SupportedLanguage::Rust => tree_sitter_rust::LANGUAGE.into(),
    };
    parser.set_language(&ts_lang)?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| anyhow!("tree-sitter parse failed"))?;
    let mut units = Vec::new();
    collect_top_level(source, tree.root_node(), &mut units);
    Ok(units)
}

fn collect_top_level(source: &[u8], root: Node<'_>, units: &mut Vec<SemanticUnit>) {
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        let kind = match child.kind() {
            "function_item" => {
                // Detect #[cfg(test)] or #[test] attributed functions as Test.
                // For now, classify by attribute presence (simple heuristic).
                UnitKind::Fn
            }
            "struct_item" => UnitKind::Struct,
            "impl_item" => UnitKind::Impl,
            "trait_item" => UnitKind::Trait,
            "enum_item" => UnitKind::Enum,
            "const_item" => UnitKind::Const,
            _ => continue,
        };
        let name = extract_name(source, child);
        let byte_range = child.byte_range();
        let start_line = child.start_position().row as u32;
        let end_line = child.end_position().row as u32;
        let content = &source[byte_range.clone()];
        let content_hash = *blake3::hash(content).as_bytes();
        units.push(SemanticUnit {
            kind,
            name,
            byte_range,
            line_range: start_line..end_line,
            content_hash,
            dependencies: Vec::new(),
        });
    }
}

fn extract_name(source: &[u8], node: Node<'_>) -> String {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" || child.kind() == "type_identifier" {
            if let Ok(s) = std::str::from_utf8(&source[child.byte_range()]) {
                return s.to_string();
            }
        }
    }
    format!("<unknown@{}>", node.start_position().row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::semantic::SupportedLanguage;

    const SIMPLE_RUST: &str = r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

struct Point {
    x: f64,
    y: f64,
}

fn main() {}
"#;

    #[test]
    fn extracts_top_level_fns_and_structs() {
        let units = extract_units(SIMPLE_RUST.as_bytes(), SupportedLanguage::Rust).unwrap();
        let names: Vec<&str> = units.iter().map(|u| u.name.as_str()).collect();
        assert!(names.contains(&"add"), "missing fn add: {names:?}");
        assert!(names.contains(&"Point"), "missing struct Point: {names:?}");
        assert!(names.contains(&"main"), "missing fn main: {names:?}");
    }

    #[test]
    fn content_hash_differs_for_different_implementations() {
        let src_a = b"fn hello() { println!(\"a\"); }";
        let src_b = b"fn hello() { println!(\"b\"); }";
        let units_a = extract_units(src_a, SupportedLanguage::Rust).unwrap();
        let units_b = extract_units(src_b, SupportedLanguage::Rust).unwrap();
        assert_eq!(units_a.len(), 1);
        assert_eq!(units_b.len(), 1);
        assert_ne!(units_a[0].content_hash, units_b[0].content_hash);
    }

    #[test]
    fn identical_implementations_have_same_hash() {
        let src = b"fn greet() { println!(\"hello\"); }";
        let units_1 = extract_units(src, SupportedLanguage::Rust).unwrap();
        let units_2 = extract_units(src, SupportedLanguage::Rust).unwrap();
        assert_eq!(units_1[0].content_hash, units_2[0].content_hash);
    }
}
