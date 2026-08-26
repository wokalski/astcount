use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use tree_sitter::Parser;
use tree_sitter_language_pack::{
    detect_language_from_content, detect_language_from_path, get_language,
};

static PROPERTIES_PARSER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CountMode {
    #[default]
    Ast,
    Named,
    All,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileMetrics {
    pub path: PathBuf,
    pub language: String,
    pub nodes: u64,
    pub ast_nodes: u64,
    pub named_nodes: u64,
    pub total_nodes: u64,
    pub errors: u64,
    pub max_depth: u32,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Totals {
    pub files: u64,
    pub nodes: u64,
    pub ast_nodes: u64,
    pub named_nodes: u64,
    pub total_nodes: u64,
    pub errors: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Report {
    pub schema: u32,
    pub tool_version: String,
    pub parser_backend: String,
    pub mode: String,
    pub totals: Totals,
    pub files: Vec<FileMetrics>,
}

#[derive(Clone, Debug)]
pub struct TimedMetrics {
    pub metrics: FileMetrics,
    pub elapsed: std::time::Duration,
}

pub fn detect_language(path: &Path, source: &[u8], forced: Option<&str>) -> Option<String> {
    forced
        .map(str::to_owned)
        .or_else(|| detect_known_language(path, source).map(str::to_owned))
}

#[must_use]
pub fn detect_known_language(path: &Path, source: &[u8]) -> Option<&'static str> {
    detect_language_from_path(&path.to_string_lossy())
        .or_else(|| language_from_filename(path))
        .or_else(|| {
            std::str::from_utf8(source)
                .ok()
                .and_then(detect_language_from_content)
        })
}

fn language_from_filename(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    match name.as_str() {
        "cmakelists.txt" => Some("cmake"),
        "makefile" | "gnumakefile" => Some("make"),
        "justfile" => Some("just"),
        "build" | "build.bazel" | "workspace" | "workspace.bazel" | "module.bazel" => {
            Some("starlark")
        }
        "jenkinsfile" => Some("groovy"),
        "gemfile" | "rakefile" | "vagrantfile" => Some("ruby"),
        _ if name == "dockerfile" || name.starts_with("dockerfile.") => Some("dockerfile"),
        _ => None,
    }
}

/// Read and measure one source file.
///
/// # Errors
///
/// Returns an error when the file cannot be read or its parser cannot be loaded.
pub fn count_file(path: &Path, language: &str, mode: CountMode) -> Result<TimedMetrics> {
    let source = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    count_source(path, language, &source, mode)
}

/// Load a parser and measure an in-memory source file.
///
/// # Errors
///
/// Returns an error when the requested parser cannot be loaded or parsing is cancelled.
pub fn count_source(
    path: &Path,
    language: &str,
    source: &[u8],
    mode: CountMode,
) -> Result<TimedMetrics> {
    let grammar = get_language(language).with_context(|| format!("load {language} parser"))?;
    let mut parser = Parser::new();
    parser
        .set_language(&grammar)
        .with_context(|| format!("configure {language} parser"))?;
    count_source_with_parser(path, language, source, mode, &mut parser)
}

/// Measure source using a previously loaded parser.
///
/// # Errors
///
/// Returns an error when parsing is cancelled.
pub fn count_source_with_parser(
    path: &Path,
    language: &str,
    source: &[u8],
    mode: CountMode,
    parser: &mut Parser,
) -> Result<TimedMetrics> {
    let started = Instant::now();
    let _scanner_guard = (language == "properties").then(|| {
        PROPERTIES_PARSER_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    });
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| anyhow!("{language} parser cancelled"))?;

    let root = tree.root_node();
    let mut cursor = root.walk();
    let mut ast_nodes = 0_u64;
    let mut named_nodes = 0_u64;
    let total_nodes = root.descendant_count() as u64;
    let mut errors = 0_u64;
    let mut depth = 0_u32;
    let mut max_depth = 0_u32;
    let has_error = root.has_error();

    'walk: loop {
        let node = cursor.node();
        let is_named = node.is_named();
        named_nodes += u64::from(is_named);
        ast_nodes += u64::from(is_named && !node.is_extra());
        if has_error {
            errors += u64::from(node.is_error() || node.is_missing());
        }
        max_depth = max_depth.max(depth);

        if cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        if cursor.goto_next_sibling() {
            continue;
        }
        loop {
            if !cursor.goto_parent() {
                break 'walk;
            }
            depth -= 1;
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }

    let nodes = match mode {
        CountMode::Ast => ast_nodes,
        CountMode::Named => named_nodes,
        CountMode::All => total_nodes,
    };
    Ok(TimedMetrics {
        metrics: FileMetrics {
            path: path.to_path_buf(),
            language: language.to_owned(),
            nodes,
            ast_nodes,
            named_nodes,
            total_nodes,
            errors,
            max_depth,
            bytes: source.len() as u64,
        },
        elapsed: started.elapsed(),
    })
}

#[must_use]
pub fn report(mut files: Vec<FileMetrics>, mode: CountMode) -> Report {
    files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    let totals = files.iter().fold(Totals::default(), |mut totals, file| {
        totals.files += 1;
        totals.nodes += file.nodes;
        totals.ast_nodes += file.ast_nodes;
        totals.named_nodes += file.named_nodes;
        totals.total_nodes += file.total_nodes;
        totals.errors += file.errors;
        totals.bytes += file.bytes;
        totals
    });
    Report {
        schema: 1,
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        parser_backend: "tree-sitter-language-pack/1.15.8".to_owned(),
        mode: match mode {
            CountMode::Ast => "ast",
            CountMode::Named => "named",
            CountMode::All => "all",
        }
        .to_owned(),
        totals,
        files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_is_sorted_and_summed() {
        let file = |path, nodes| FileMetrics {
            path: PathBuf::from(path),
            language: "rust".to_owned(),
            nodes,
            ast_nodes: nodes,
            named_nodes: nodes,
            total_nodes: nodes + 1,
            errors: 0,
            max_depth: 2,
            bytes: 10,
        };
        let result = report(vec![file("z.rs", 3), file("a.rs", 2)], CountMode::Named);
        assert_eq!(result.files[0].path, PathBuf::from("a.rs"));
        assert_eq!(result.totals.nodes, 5);
        assert_eq!(result.totals.files, 2);
        assert_eq!(result.totals.bytes, 20);
    }

    #[test]
    fn detects_conventional_filenames_and_shebangs() {
        assert_eq!(
            detect_language(Path::new("Dockerfile.dev"), b"", None).as_deref(),
            Some("dockerfile")
        );
        assert_eq!(
            detect_language(Path::new("Makefile"), b"", None).as_deref(),
            Some("make")
        );
        assert_eq!(
            detect_language(Path::new("tool"), b"#!/usr/bin/env python3\n", None).as_deref(),
            Some("python")
        );
    }

    #[test]
    #[ignore = "downloads the Rust grammar on first use"]
    fn counts_a_real_rust_tree() {
        let source = b"fn answer() -> u32 { 40 + 2 }\n";
        let result = count_source(Path::new("answer.rs"), "rust", source, CountMode::Ast)
            .expect("parse Rust");
        assert_eq!(result.metrics.errors, 0);
        assert!(result.metrics.ast_nodes > 5);
        assert!(result.metrics.total_nodes > result.metrics.ast_nodes);
        assert_eq!(result.metrics.nodes, result.metrics.ast_nodes);
    }
}
