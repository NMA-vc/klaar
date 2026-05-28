use std::path::Path;
use tree_sitter::{Language, Node, Parser};

// ---------------------------------------------------------------------------
// Language detection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    TypeScript,
    Python,
    Go,
}

pub fn detect_language(path: &Path) -> Option<Lang> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => Some(Lang::Rust),
        Some("ts") | Some("tsx") => Some(Lang::TypeScript),
        Some("py") => Some(Lang::Python),
        Some("go") => Some(Lang::Go),
        _ => None,
    }
}

fn ts_language(lang: Lang) -> Language {
    match lang {
        Lang::Rust => tree_sitter_rust::language(),
        Lang::TypeScript => tree_sitter_typescript::language_typescript(),
        Lang::Python => tree_sitter_python::language(),
        Lang::Go => tree_sitter_go::language(),
    }
}

// ---------------------------------------------------------------------------
// Skeleton extraction
// ---------------------------------------------------------------------------
thread_local! {
    static TS_PARSER: std::cell::RefCell<Parser> = std::cell::RefCell::new(Parser::new());
}

/// Extract only signatures/definitions from `source`, annotated with
/// original line numbers. Bodies are stripped. Returns the skeleton string.
pub fn extract_skeleton(source: &str, lang: Lang) -> String {
    let tree = TS_PARSER.with(|cached_parser| {
        let mut parser = cached_parser.borrow_mut();
        parser
            .set_language(&ts_language(lang))
            .expect("tree-sitter language init failed");
        
        parser.parse(source, None)
    });

    let tree = match tree {
        Some(t) => t,
        None => return format!("// parse failed\n{}", source),
    };

    let lines: Vec<&str> = source.lines().collect();
    let mut out = String::new();

    collect_skeleton_nodes(tree.root_node(), source, &lines, lang, &mut out, 0);

    if out.is_empty() {
        "// (skeleton: no top-level definitions found)\n".to_string()
    } else {
        out
    }
}

/// Remove all comments from the source while preserving structure and whitespace.
pub fn strip_comments(source: &str, lang: Lang) -> String {
    let tree = TS_PARSER.with(|cached_parser| {
        let mut parser = cached_parser.borrow_mut();
        parser
            .set_language(&ts_language(lang))
            .expect("tree-sitter language init failed");
        
        parser.parse(source, None)
    });

    let tree = match tree {
        Some(t) => t,
        None => return source.to_string(),
    };

    let mut comment_spans = Vec::new();
    find_comment_spans(tree.root_node(), &mut comment_spans);

    // Sort spans by start position and remove them in reverse to keep indices valid
    comment_spans.sort_by_key(|(start, _)| *start);
    
    let mut result = source.to_string();
    for (start, end) in comment_spans.into_iter().rev() {
        // Only remove if it's a full line or if it doesn't leave dangling syntax.
        // For now, let's just remove the text at those spans.
        result.replace_range(start..end, "");
    }

    // Clean up: collapse runs of 2+ consecutive blank lines into a single blank line
    let mut final_lines: Vec<&str> = Vec::new();
    let mut prev_blank = false;
    for line in result.lines() {
        let is_blank = line.trim().is_empty();
        if is_blank && prev_blank {
            continue; // skip consecutive blank lines
        }
        final_lines.push(line);
        prev_blank = is_blank;
    }
    final_lines.join("\n")
}

fn find_comment_spans(node: Node, spans: &mut Vec<(usize, usize)>) {
    if node.kind().contains("comment") {
        spans.push((node.start_byte(), node.end_byte()));
    } else {
        let mut walker = node.walk();
        for child in node.children(&mut walker) {
            find_comment_spans(child, spans);
        }
    }
}

#[derive(Debug)]
pub struct Symbol {
    pub name: String,
    pub line: usize,
    pub kind: String,
}

/// Extract names and line numbers of all top-level symbols in the file.
pub fn extract_symbols(source: &str, lang: Lang) -> Vec<Symbol> {
    let tree = TS_PARSER.with(|cached_parser| {
        let mut parser = cached_parser.borrow_mut();
        parser
            .set_language(&ts_language(lang))
            .expect("tree-sitter language init failed");
        
        parser.parse(source, None)
    });

    let tree = match tree {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut symbols = Vec::new();
    collect_symbol_metadata(tree.root_node(), source, lang, &mut symbols, 0);
    symbols
}

fn collect_symbol_metadata(
    node: Node,
    source: &str,
    lang: Lang,
    symbols: &mut Vec<Symbol>,
    depth: usize,
) {
    if is_definition_node(node, lang) {
        // Try to find the identifier child
        let mut name = "anonymous".to_string();
        let mut walker = node.walk();
        for child in node.children(&mut walker) {
            if child.kind().contains("identifier") || child.kind() == "name" {
                if let Ok(n) = child.utf8_text(source.as_bytes()) {
                    name = n.to_string();
                    break;
                }
            }
        }

        symbols.push(Symbol {
            name,
            line: node.start_position().row + 1,
            kind: node.kind().to_string(),
        });
        return;
    }

    if is_container_node(node, lang) || depth == 0 {
        for child in node.children(&mut node.walk()) {
            collect_symbol_metadata(child, source, lang, symbols, depth + 1);
        }
    }
}

/// Walk top-level nodes and emit signature lines.
fn collect_skeleton_nodes(
    node: Node,
    source: &str,
    lines: &[&str],
    lang: Lang,
    out: &mut String,
    depth: usize,
) {
    // Only recurse into module/source level at top, and into impl blocks 1 level deep
    let eligible = is_definition_node(node, lang);

    if eligible {
        let start_line = node.start_position().row; // 0-indexed
        let sig = extract_signature(node, source, lines, lang);
        out.push_str(&format!("// L{}\n{}\n\n", start_line + 1, sig));
        return;
    }

    // Recurse into containers (impl blocks, modules, source file)
    if is_container_node(node, lang) {
        for child in node.children(&mut node.walk()) {
            collect_skeleton_nodes(child, source, lines, lang, out, depth + 1);
        }
    } else if depth == 0 {
        // top-level: always recurse into source_file
        for child in node.children(&mut node.walk()) {
            collect_skeleton_nodes(child, source, lines, lang, out, depth + 1);
        }
    }
}

fn is_definition_node(node: Node, lang: Lang) -> bool {
    let kind = node.kind();
    match lang {
        Lang::Rust => matches!(
            kind,
            "function_item"
                | "struct_item"
                | "enum_item"
                | "trait_item"
                | "type_item"
                | "const_item"
                | "static_item"
                | "use_declaration"
        ),
        Lang::TypeScript => matches!(
            kind,
            "function_declaration"
                | "export_statement"
                | "class_declaration"
                | "interface_declaration"
                | "type_alias_declaration"
                | "enum_declaration"
                | "lexical_declaration"
        ),
        Lang::Python => matches!(
            kind,
            "function_definition" | "class_definition" | "decorated_definition"
        ),
        Lang::Go => matches!(
            kind,
            "function_declaration"
                | "method_declaration"
                | "type_declaration"
                | "var_declaration"
                | "const_declaration"
        ),
    }
}

fn is_container_node(node: Node, lang: Lang) -> bool {
    let kind = node.kind();
    match lang {
        Lang::Rust => matches!(kind, "impl_item" | "mod_item" | "source_file"),
        Lang::TypeScript => matches!(kind, "program" | "module"),
        Lang::Python => matches!(kind, "module"),
        Lang::Go => matches!(kind, "source_file"),
    }
}

/// Extract just the signature line(s) of a node — everything up to the body.
fn extract_signature(node: Node, _source: &str, lines: &[&str], lang: Lang) -> String {
    // Find the body node child
    let body_kinds = match lang {
        Lang::Rust => &["block", "declaration_list", "enum_variant_list", "field_declaration_list"][..],
        Lang::TypeScript => &["statement_block", "class_body"][..],
        Lang::Python => &["block"][..],
        Lang::Go => &["block"][..],
    };

    let start_row = node.start_position().row;
    let mut end_row = node.end_position().row;

    // Walk children to find where the body starts
    let mut walker = node.walk();
    for child in node.children(&mut walker) {
        if body_kinds.contains(&child.kind()) {
            // Body starts here — emit up to but not including the body line
            let body_start = child.start_position().row;
            end_row = if body_start > start_row {
                body_start - 1
            } else {
                start_row
            };
            break;
        }
    }

    // Collect the signature lines
    let sig_lines: Vec<&str> = lines
        .get(start_row..=end_row.min(lines.len().saturating_sub(1)))
        .unwrap_or(&[]).to_vec();

    let sig = sig_lines.join("\n").trim_end().to_string();
    if sig.is_empty() {
        lines.get(start_row).copied().unwrap_or("").to_string()
    } else {
        sig
    }
}
