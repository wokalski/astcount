use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use tree_sitter::Parser;
use tree_sitter_language_pack::{
    detect_language_from_content, detect_language_from_path, get_language,
};

static PROPERTIES_PARSER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeProperty {
    Named,
    Extra,
    Error,
    Missing,
}

impl NodeProperty {
    const fn mask(self) -> u8 {
        match self {
            Self::Named => 1 << 0,
            Self::Extra => 1 << 1,
            Self::Error => 1 << 2,
            Self::Missing => 1 << 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NodeSelection {
    require: u8,
    exclude: u8,
}

impl NodeSelection {
    /// Build a conjunction of Tree-sitter node-property predicates.
    ///
    /// # Errors
    ///
    /// Returns an error if the same property is both required and excluded.
    pub fn new(require: &[NodeProperty], exclude: &[NodeProperty]) -> Result<Self> {
        let require = require
            .iter()
            .fold(0, |mask, property| mask | property.mask());
        let exclude = exclude
            .iter()
            .fold(0, |mask, property| mask | property.mask());
        if require & exclude != 0 {
            return Err(anyhow!(
                "a node property cannot be both required and excluded"
            ));
        }
        Ok(Self { require, exclude })
    }

    const fn matches(self, properties: u8) -> bool {
        properties & self.require == self.require && properties & self.exclude == 0
    }

    fn properties(mask: u8) -> Vec<NodeProperty> {
        [
            NodeProperty::Named,
            NodeProperty::Extra,
            NodeProperty::Error,
            NodeProperty::Missing,
        ]
        .into_iter()
        .filter(|property| mask & property.mask() != 0)
        .collect()
    }

    fn report(self) -> Selection {
        Selection {
            require: Self::properties(self.require),
            exclude: Self::properties(self.exclude),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Selection {
    pub require: Vec<NodeProperty>,
    pub exclude: Vec<NodeProperty>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileMetrics {
    pub path: PathBuf,
    pub language: String,
    pub nodes: u64,
    pub total_nodes: u64,
    pub named_nodes: u64,
    pub extra_nodes: u64,
    pub error_nodes: u64,
    pub missing_nodes: u64,
    pub max_depth: u32,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Totals {
    pub files: u64,
    pub nodes: u64,
    pub total_nodes: u64,
    pub named_nodes: u64,
    pub extra_nodes: u64,
    pub error_nodes: u64,
    pub missing_nodes: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Report {
    pub schema: u32,
    pub tool_version: String,
    pub parser_backend: String,
    pub selection: Selection,
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
pub fn count_file(path: &Path, language: &str, selection: NodeSelection) -> Result<TimedMetrics> {
    let source = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    count_source(path, language, &source, selection)
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
    selection: NodeSelection,
) -> Result<TimedMetrics> {
    let grammar = get_language(language).with_context(|| format!("load {language} parser"))?;
    let mut parser = Parser::new();
    parser
        .set_language(&grammar)
        .with_context(|| format!("configure {language} parser"))?;
    count_source_with_parser(path, language, source, selection, &mut parser)
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
    selection: NodeSelection,
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
    let mut named_nodes = 0_u64;
    let total_nodes = root.descendant_count() as u64;
    let mut extra_nodes = 0_u64;
    let mut error_nodes = 0_u64;
    let mut missing_nodes = 0_u64;
    let mut selected_nodes = 0_u64;
    let mut depth = 0_u32;
    let mut max_depth = 0_u32;
    let has_error = root.has_error();

    'walk: loop {
        let node = cursor.node();
        let is_named = node.is_named();
        let is_extra = node.is_extra();
        let is_error = has_error && node.is_error();
        let is_missing = has_error && node.is_missing();
        let properties = u8::from(is_named)
            | (u8::from(is_extra) << 1)
            | (u8::from(is_error) << 2)
            | (u8::from(is_missing) << 3);
        selected_nodes += u64::from(selection.matches(properties));
        named_nodes += u64::from(is_named);
        extra_nodes += u64::from(is_extra);
        error_nodes += u64::from(is_error);
        missing_nodes += u64::from(is_missing);
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

    Ok(TimedMetrics {
        metrics: FileMetrics {
            path: path.to_path_buf(),
            language: language.to_owned(),
            nodes: selected_nodes,
            total_nodes,
            named_nodes,
            extra_nodes,
            error_nodes,
            missing_nodes,
            max_depth,
            bytes: source.len() as u64,
        },
        elapsed: started.elapsed(),
    })
}

#[must_use]
pub fn report(mut files: Vec<FileMetrics>, selection: NodeSelection) -> Report {
    files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    let totals = files.iter().fold(Totals::default(), |mut totals, file| {
        totals.files += 1;
        totals.nodes += file.nodes;
        totals.total_nodes += file.total_nodes;
        totals.named_nodes += file.named_nodes;
        totals.extra_nodes += file.extra_nodes;
        totals.error_nodes += file.error_nodes;
        totals.missing_nodes += file.missing_nodes;
        totals.bytes += file.bytes;
        totals
    });
    Report {
        schema: 2,
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        parser_backend: "tree-sitter-language-pack/1.15.8".to_owned(),
        selection: selection.report(),
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
            total_nodes: nodes + 1,
            named_nodes: nodes,
            extra_nodes: 0,
            error_nodes: 0,
            missing_nodes: 0,
            max_depth: 2,
            bytes: 10,
        };
        let selection = NodeSelection::new(&[NodeProperty::Named], &[]).unwrap();
        let result = report(vec![file("z.rs", 3), file("a.rs", 2)], selection);
        assert_eq!(result.files[0].path, PathBuf::from("a.rs"));
        assert_eq!(result.totals.nodes, 5);
        assert_eq!(result.totals.files, 2);
        assert_eq!(result.totals.bytes, 20);
    }

    #[test]
    fn report_json_exposes_selection_and_primitive_totals() {
        let selection = NodeSelection::new(&[NodeProperty::Named], &[NodeProperty::Extra]).unwrap();
        let result = report(
            vec![FileMetrics {
                path: PathBuf::from("fixture.rs"),
                language: "rust".to_owned(),
                nodes: 8,
                total_nodes: 18,
                named_nodes: 9,
                extra_nodes: 1,
                error_nodes: 0,
                missing_nodes: 0,
                max_depth: 4,
                bytes: 35,
            }],
            selection,
        );
        let json = serde_json::to_value(result).expect("serialize report");
        assert_eq!(json["schema"], 2);
        assert_eq!(json["selection"]["require"][0], "named");
        assert_eq!(json["selection"]["exclude"][0], "extra");
        assert_eq!(json["totals"]["nodes"], 8);
        assert_eq!(json["totals"]["total_nodes"], 18);
        assert_eq!(json["totals"]["named_nodes"], 9);
        assert_eq!(json["totals"]["extra_nodes"], 1);
        assert_eq!(json["totals"]["error_nodes"], 0);
        assert_eq!(json["totals"]["missing_nodes"], 0);
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
    fn golden_counts_pinned_rust_syntax_trees() {
        struct Golden {
            name: &'static str,
            source: &'static [u8],
            named_nodes: u64,
            total_nodes: u64,
            extra_nodes: u64,
            error_nodes: u64,
            missing_nodes: u64,
            max_depth: u32,
        }

        let fixtures = [
            Golden {
                name: "named extra comment",
                source: b"// note\nfn main() { let x = 1; }\n",
                named_nodes: 9,
                total_nodes: 18,
                extra_nodes: 1,
                error_nodes: 0,
                missing_nodes: 0,
                max_depth: 4,
            },
            Golden {
                name: "error recovery node",
                source: b"fn main( {\n",
                named_nodes: 3,
                total_nodes: 6,
                extra_nodes: 1,
                error_nodes: 1,
                missing_nodes: 0,
                max_depth: 2,
            },
            Golden {
                name: "missing semicolon node",
                source: b"fn main() { let x = 1 }\n",
                named_nodes: 8,
                total_nodes: 16,
                extra_nodes: 0,
                error_nodes: 0,
                missing_nodes: 1,
                max_depth: 4,
            },
        ];

        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("configure pinned Rust parser");

        for fixture in fixtures {
            for (selection, nodes) in [
                (NodeSelection::default(), fixture.total_nodes),
                (
                    NodeSelection::new(&[NodeProperty::Named], &[]).unwrap(),
                    fixture.named_nodes,
                ),
                (
                    NodeSelection::new(&[NodeProperty::Extra], &[]).unwrap(),
                    fixture.extra_nodes,
                ),
                (
                    NodeSelection::new(&[NodeProperty::Error], &[]).unwrap(),
                    fixture.error_nodes,
                ),
                (
                    NodeSelection::new(&[NodeProperty::Missing], &[]).unwrap(),
                    fixture.missing_nodes,
                ),
                (
                    NodeSelection::new(&[], &[NodeProperty::Named]).unwrap(),
                    fixture.total_nodes - fixture.named_nodes,
                ),
            ] {
                let actual = count_source_with_parser(
                    Path::new("fixture.rs"),
                    "rust",
                    fixture.source,
                    selection,
                    &mut parser,
                )
                .expect("count pinned Rust fixture")
                .metrics;
                let expected = FileMetrics {
                    path: PathBuf::from("fixture.rs"),
                    language: "rust".to_owned(),
                    nodes,
                    total_nodes: fixture.total_nodes,
                    named_nodes: fixture.named_nodes,
                    extra_nodes: fixture.extra_nodes,
                    error_nodes: fixture.error_nodes,
                    missing_nodes: fixture.missing_nodes,
                    max_depth: fixture.max_depth,
                    bytes: fixture.source.len() as u64,
                };
                assert_eq!(
                    actual, expected,
                    "{} with selection {selection:?}",
                    fixture.name
                );
            }
        }
    }

    #[test]
    fn rejects_contradictory_selection() {
        assert!(NodeSelection::new(&[NodeProperty::Extra], &[NodeProperty::Extra]).is_err());
    }
}
