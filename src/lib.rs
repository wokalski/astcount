use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use tree_sitter::Parser;
use tree_sitter_language_pack::{
    detect_language_from_content, detect_language_from_path, get_language,
};

static PROPERTIES_PARSER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[repr(u8)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Named = 1 << 0,
    Anonymous = 1 << 1,
    Extra = 1 << 2,
    Error = 1 << 3,
    Missing = 1 << 4,
}

impl NodeKind {
    const fn mask(self) -> u8 {
        self as u8
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Named => "named",
            Self::Anonymous => "anonymous",
            Self::Extra => "extra",
            Self::Error => "error",
            Self::Missing => "missing",
        }
    }
}

impl fmt::Display for NodeKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NodeFilter {
    excluded: u8,
}

#[derive(Clone, Copy, Debug)]
struct NodeTraits(u8);

impl NodeFilter {
    /// Build a filter that removes nodes matching any excluded kind.
    ///
    /// # Errors
    ///
    /// Returns an error when both named and anonymous nodes are excluded.
    pub fn excluding(excluded: &[NodeKind]) -> Result<Self> {
        let excluded = excluded.iter().fold(0, |mask, kind| mask | kind.mask());
        ensure!(
            excluded & NodeKind::Named.mask() == 0 || excluded & NodeKind::Anonymous.mask() == 0,
            "cannot exclude both named and anonymous nodes"
        );
        Ok(Self { excluded })
    }

    const fn includes(self, traits: NodeTraits) -> bool {
        !((traits.0 & NodeKind::Named.mask() != 0 && self.excluded & NodeKind::Named.mask() != 0)
            || (traits.0 & NodeKind::Named.mask() == 0
                && self.excluded & NodeKind::Anonymous.mask() != 0)
            || (traits.0 & NodeKind::Extra.mask() != 0
                && self.excluded & NodeKind::Extra.mask() != 0)
            || (traits.0 & NodeKind::Error.mask() != 0
                && self.excluded & NodeKind::Error.mask() != 0)
            || (traits.0 & NodeKind::Missing.mask() != 0
                && self.excluded & NodeKind::Missing.mask() != 0))
    }

    fn kinds(self) -> Vec<NodeKind> {
        [
            NodeKind::Named,
            NodeKind::Anonymous,
            NodeKind::Extra,
            NodeKind::Error,
            NodeKind::Missing,
        ]
        .into_iter()
        .filter(|kind| self.excluded & kind.mask() != 0)
        .collect()
    }

    fn report(self) -> Filter {
        Filter {
            excluded: self.kinds(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Filter {
    pub excluded: Vec<NodeKind>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PropertyCounts {
    pub named: u64,
    pub extra: u64,
    pub error: u64,
    pub missing: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct NodeCounts {
    pub selected: u64,
    pub total: u64,
    pub by_property: PropertyCounts,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileMetrics {
    pub path: PathBuf,
    pub language: String,
    pub nodes: NodeCounts,
    pub max_depth: u32,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Totals {
    pub files: u64,
    pub bytes: u64,
    pub nodes: NodeCounts,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Report {
    pub schema: u32,
    pub tool_version: String,
    pub parser_backend: String,
    pub filter: Filter,
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
pub fn count_file(path: &Path, language: &str, filter: NodeFilter) -> Result<TimedMetrics> {
    let source = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    count_source(path, language, &source, filter)
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
    filter: NodeFilter,
) -> Result<TimedMetrics> {
    let grammar = get_language(language).with_context(|| format!("load {language} parser"))?;
    let mut parser = Parser::new();
    parser
        .set_language(&grammar)
        .with_context(|| format!("configure {language} parser"))?;
    count_source_with_parser(path, language, source, filter, &mut parser)
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
    filter: NodeFilter,
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
        let traits = (u8::from(is_named) * NodeKind::Named.mask())
            | (u8::from(is_extra) * NodeKind::Extra.mask())
            | (u8::from(is_error) * NodeKind::Error.mask())
            | (u8::from(is_missing) * NodeKind::Missing.mask());
        selected_nodes += u64::from(filter.includes(NodeTraits(traits)));
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
            nodes: NodeCounts {
                selected: selected_nodes,
                total: total_nodes,
                by_property: PropertyCounts {
                    named: named_nodes,
                    extra: extra_nodes,
                    error: error_nodes,
                    missing: missing_nodes,
                },
            },
            max_depth,
            bytes: source.len() as u64,
        },
        elapsed: started.elapsed(),
    })
}

#[must_use]
pub fn report(mut files: Vec<FileMetrics>, filter: NodeFilter) -> Report {
    files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    let totals = files.iter().fold(Totals::default(), |mut totals, file| {
        totals.files += 1;
        totals.nodes.selected += file.nodes.selected;
        totals.nodes.total += file.nodes.total;
        totals.nodes.by_property.named += file.nodes.by_property.named;
        totals.nodes.by_property.extra += file.nodes.by_property.extra;
        totals.nodes.by_property.error += file.nodes.by_property.error;
        totals.nodes.by_property.missing += file.nodes.by_property.missing;
        totals.bytes += file.bytes;
        totals
    });
    Report {
        schema: 3,
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        parser_backend: "tree-sitter-language-pack/1.15.8".to_owned(),
        filter: filter.report(),
        totals,
        files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_is_sorted_and_summed() {
        let file = |path, selected| FileMetrics {
            path: PathBuf::from(path),
            language: "rust".to_owned(),
            nodes: NodeCounts {
                selected,
                total: selected + 1,
                by_property: PropertyCounts {
                    named: selected,
                    ..PropertyCounts::default()
                },
            },
            max_depth: 2,
            bytes: 10,
        };
        let filter = NodeFilter::excluding(&[NodeKind::Anonymous]).unwrap();
        let result = report(vec![file("z.rs", 3), file("a.rs", 2)], filter);
        assert_eq!(result.files[0].path, PathBuf::from("a.rs"));
        assert_eq!(result.totals.nodes.selected, 5);
        assert_eq!(result.totals.nodes.total, 7);
        assert_eq!(result.totals.files, 2);
        assert_eq!(result.totals.bytes, 20);
    }

    #[test]
    fn report_json_groups_filter_and_node_counts() {
        let filter = NodeFilter::excluding(&[NodeKind::Anonymous, NodeKind::Extra]).unwrap();
        let result = report(
            vec![FileMetrics {
                path: PathBuf::from("fixture.rs"),
                language: "rust".to_owned(),
                nodes: NodeCounts {
                    selected: 8,
                    total: 18,
                    by_property: PropertyCounts {
                        named: 9,
                        extra: 1,
                        error: 0,
                        missing: 0,
                    },
                },
                max_depth: 4,
                bytes: 35,
            }],
            filter,
        );
        let json = serde_json::to_value(result).expect("serialize report");
        assert_eq!(json["schema"], 3);
        assert_eq!(json["filter"]["excluded"][0], "anonymous");
        assert_eq!(json["filter"]["excluded"][1], "extra");
        assert_eq!(json["totals"]["nodes"]["selected"], 8);
        assert_eq!(json["totals"]["nodes"]["total"], 18);
        assert_eq!(json["totals"]["nodes"]["by_property"]["named"], 9);
        assert_eq!(json["totals"]["nodes"]["by_property"]["extra"], 1);
        assert_eq!(json["totals"]["nodes"]["by_property"]["error"], 0);
        assert_eq!(json["totals"]["nodes"]["by_property"]["missing"], 0);
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
            for (filter, selected) in [
                (NodeFilter::default(), fixture.total_nodes),
                (
                    NodeFilter::excluding(&[NodeKind::Anonymous]).unwrap(),
                    fixture.named_nodes,
                ),
                (
                    NodeFilter::excluding(&[NodeKind::Named]).unwrap(),
                    fixture.total_nodes - fixture.named_nodes,
                ),
                (
                    NodeFilter::excluding(&[NodeKind::Extra]).unwrap(),
                    fixture.total_nodes - fixture.extra_nodes,
                ),
                (
                    NodeFilter::excluding(&[NodeKind::Error]).unwrap(),
                    fixture.total_nodes - fixture.error_nodes,
                ),
                (
                    NodeFilter::excluding(&[NodeKind::Missing]).unwrap(),
                    fixture.total_nodes - fixture.missing_nodes,
                ),
            ] {
                let actual = count_source_with_parser(
                    Path::new("fixture.rs"),
                    "rust",
                    fixture.source,
                    filter,
                    &mut parser,
                )
                .expect("count pinned Rust fixture")
                .metrics;
                let expected = FileMetrics {
                    path: PathBuf::from("fixture.rs"),
                    language: "rust".to_owned(),
                    nodes: NodeCounts {
                        selected,
                        total: fixture.total_nodes,
                        by_property: PropertyCounts {
                            named: fixture.named_nodes,
                            extra: fixture.extra_nodes,
                            error: fixture.error_nodes,
                            missing: fixture.missing_nodes,
                        },
                    },
                    max_depth: fixture.max_depth,
                    bytes: fixture.source.len() as u64,
                };
                assert_eq!(actual, expected, "{} with filter {filter:?}", fixture.name);
            }
        }
    }

    #[test]
    fn rejects_excluding_every_namedness() {
        assert!(NodeFilter::excluding(&[NodeKind::Named, NodeKind::Anonymous]).is_err());
    }
}
