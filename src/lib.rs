use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, ensure};
use ast_grep_core::AstGrep;
use ast_grep_core::language::Language as AstGrepLanguage;
use ast_grep_core::matcher::{Pattern, PatternBuilder, PatternError};
use ast_grep_core::tree_sitter::{LanguageExt as AstGrepLanguageExt, StrDoc};
use serde::{Deserialize, Serialize};
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator, Tree};
use tree_sitter_language_pack::{
    detect_language_from_content, detect_language_from_path, get_language,
};

static PROPERTIES_PARSER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub const REPORT_SCHEMA: u32 = 4;
pub const AST_GREP_BACKEND: &str = "ast-grep-core/0.45.2";

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
            ..Filter::default()
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct LanguageFilter {
    pub language: String,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct LanguageNodeType {
    pub language: String,

    #[serde(rename = "type")]
    pub node_type: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Filter {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_types: Vec<LanguageNodeType>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tree_sitter_selectors: Vec<LanguageFilter>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ast_grep_selectors: Vec<LanguageFilter>,

    #[serde(default)]
    pub excluded: Vec<NodeKind>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_files: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tree_sitter_queries: Vec<LanguageFilter>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ast_grep_patterns: Vec<LanguageFilter>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presets: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ast_grep_backend: Option<String>,
}

impl Filter {
    /// Build the executable node-kind filter recorded in this report filter.
    ///
    /// # Errors
    ///
    /// Returns an error when both named and anonymous nodes are excluded.
    pub fn node_filter(&self) -> Result<NodeFilter> {
        NodeFilter::excluding(&self.excluded)
    }

    #[must_use]
    pub fn has_selectors(&self) -> bool {
        !self.selected_types.is_empty()
            || !self.tree_sitter_selectors.is_empty()
            || !self.ast_grep_selectors.is_empty()
    }
}

#[derive(Default)]
struct MatchedNodeIds {
    selected: HashSet<usize>,
    excluded: HashSet<usize>,
}

/// Language-specific selectors and exclusions compiled for one parser.
#[derive(Default)]
pub struct CompiledFilters {
    selection_active: bool,
    selected_types: HashSet<String>,
    selector_queries: Vec<Query>,
    exclusion_queries: Vec<Query>,
    selector_patterns: Vec<Pattern>,
    exclusion_patterns: Vec<Pattern>,
    ast_grep_language: Option<PatternLanguage>,
}

impl CompiledFilters {
    /// Compile every filter that targets `language` against its exact grammar.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid Tree-sitter query, a query without its
    /// required capture, or an invalid ast-grep pattern.
    pub fn compile(language: &str, grammar: &Language, filter: &Filter) -> Result<Self> {
        let selector_queries =
            compile_queries(language, grammar, &filter.tree_sitter_selectors, "select")?;
        let exclusion_queries =
            compile_queries(language, grammar, &filter.tree_sitter_queries, "exclude")?;
        let selector_pattern_sources = filter
            .ast_grep_selectors
            .iter()
            .filter(|pattern| pattern.language.eq_ignore_ascii_case(language))
            .collect::<Vec<_>>();
        let exclusion_pattern_sources = filter
            .ast_grep_patterns
            .iter()
            .filter(|pattern| pattern.language.eq_ignore_ascii_case(language))
            .collect::<Vec<_>>();
        let ast_grep_language = (!selector_pattern_sources.is_empty()
            || !exclusion_pattern_sources.is_empty())
        .then(|| PatternLanguage {
            grammar: grammar.clone(),
            expando: ast_grep_expando(language),
        });
        let mut selector_patterns = Vec::with_capacity(selector_pattern_sources.len());
        let mut exclusion_patterns = Vec::with_capacity(exclusion_pattern_sources.len());
        if let Some(ast_language) = &ast_grep_language {
            for pattern_source in selector_pattern_sources {
                selector_patterns.push(
                    Pattern::try_new(&pattern_source.source, ast_language.clone()).with_context(
                        || {
                            format!(
                                "compile ast-grep selector pattern for {}",
                                pattern_source.language
                            )
                        },
                    )?,
                );
            }
            for pattern_source in exclusion_pattern_sources {
                exclusion_patterns.push(
                    Pattern::try_new(&pattern_source.source, ast_language.clone()).with_context(
                        || {
                            format!(
                                "compile ast-grep exclusion pattern for {}",
                                pattern_source.language
                            )
                        },
                    )?,
                );
            }
        }

        let selected_types = filter
            .selected_types
            .iter()
            .filter(|selected| selected.language.eq_ignore_ascii_case(language))
            .map(|selected| selected.node_type.clone())
            .collect::<HashSet<_>>();
        for node_type in &selected_types {
            ensure!(
                (0..grammar.node_kind_count()).any(|id| {
                    u16::try_from(id)
                        .ok()
                        .and_then(|id| grammar.node_kind_for_id(id))
                        .is_some_and(|kind| kind == node_type)
                }),
                "unknown Tree-sitter node type {node_type:?} for {language}"
            );
        }

        Ok(Self {
            selection_active: filter.has_selectors(),
            selected_types,
            selector_queries,
            exclusion_queries,
            selector_patterns,
            exclusion_patterns,
            ast_grep_language,
        })
    }

    fn matched_node_ids(&self, tree: &Tree, source: &[u8]) -> Result<MatchedNodeIds> {
        let mut matched = MatchedNodeIds::default();
        capture_node_ids(
            &self.selector_queries,
            "select",
            tree,
            source,
            &mut matched.selected,
        )?;
        capture_node_ids(
            &self.exclusion_queries,
            "exclude",
            tree,
            source,
            &mut matched.excluded,
        )?;

        if self.selector_patterns.is_empty() && self.exclusion_patterns.is_empty() {
            return Ok(matched);
        }

        if let Some(language) = &self.ast_grep_language {
            let source = std::str::from_utf8(source)
                .context("ast-grep patterns require UTF-8 source text")?;
            let document = StrDoc {
                src: source.to_owned(),
                lang: language.clone(),
                tree: tree.clone(),
            };
            let ast = AstGrep::doc(document);
            let root = ast.root();
            for pattern in &self.selector_patterns {
                for selected in root.find_all(pattern.clone()) {
                    matched.selected.insert(selected.get_node().node_id());
                }
            }
            for pattern in &self.exclusion_patterns {
                for excluded in root.find_all(pattern.clone()) {
                    matched.excluded.insert(excluded.get_node().node_id());
                }
            }
        }
        Ok(matched)
    }

    fn selects(&self, node_type: &str, node_id: usize, matched: &MatchedNodeIds) -> bool {
        !self.selection_active
            || self.selected_types.contains(node_type)
            || matched.selected.contains(&node_id)
    }
}

fn compile_queries(
    language: &str,
    grammar: &Language,
    sources: &[LanguageFilter],
    capture_name: &str,
) -> Result<Vec<Query>> {
    let mut queries = Vec::new();
    for source in sources
        .iter()
        .filter(|source| source.language.eq_ignore_ascii_case(language))
    {
        let query = Query::new(grammar, &source.source)
            .with_context(|| format!("compile Tree-sitter query for {}", source.language))?;
        let target = if capture_name == "select" {
            "selected nodes"
        } else {
            "excluded subtree roots"
        };
        ensure!(
            query.capture_names().contains(&capture_name),
            "Tree-sitter query for {} must capture {target} as @{capture_name}",
            source.language,
        );
        queries.push(query);
    }
    Ok(queries)
}

fn capture_node_ids(
    queries: &[Query],
    capture_name: &str,
    tree: &Tree,
    source: &[u8],
    node_ids: &mut HashSet<usize>,
) -> Result<()> {
    for query in queries {
        let wanted_capture = query
            .capture_index_for_name(capture_name)
            .expect("validated query capture");
        let mut cursor = QueryCursor::new();
        let mut captures = cursor.captures(query, tree.root_node(), source);
        while let Some((query_match, capture_index)) = captures.next() {
            let capture = query_match.captures[*capture_index];
            if capture.index == wanted_capture {
                node_ids.insert(capture.node.id());
            }
        }
        ensure!(
            !cursor.did_exceed_match_limit(),
            "Tree-sitter query exceeded its match limit"
        );
    }
    Ok(())
}

#[derive(Clone)]
struct PatternLanguage {
    grammar: Language,
    expando: char,
}

impl AstGrepLanguage for PatternLanguage {
    fn kind_to_id(&self, kind: &str) -> u16 {
        self.grammar.id_for_node_kind(kind, true)
    }

    fn field_to_id(&self, field: &str) -> Option<u16> {
        self.grammar.field_id_for_name(field).map(Into::into)
    }

    fn expando_char(&self) -> char {
        self.expando
    }

    fn pre_process_pattern<'query>(&self, query: &'query str) -> Cow<'query, str> {
        preprocess_ast_grep_pattern(self.expando, query)
    }

    fn build_pattern(&self, builder: &PatternBuilder) -> Result<Pattern, PatternError> {
        builder.build(|source| StrDoc::try_new(source, self.clone()))
    }
}

impl AstGrepLanguageExt for PatternLanguage {
    fn get_ts_language(&self) -> Language {
        self.grammar.clone()
    }
}

fn preprocess_ast_grep_pattern(expando: char, query: &str) -> Cow<'_, str> {
    if expando == '$' {
        return Cow::Borrowed(query);
    }
    let mut output = String::with_capacity(query.len());
    let mut dollars = 0;
    for character in query.chars() {
        if character == '$' {
            dollars += 1;
            continue;
        }
        let replacement = if matches!(character, 'A'..='Z' | '_') || dollars == 3 {
            expando
        } else {
            '$'
        };
        output.extend(std::iter::repeat_n(replacement, dollars));
        dollars = 0;
        output.push(character);
    }
    let replacement = if dollars == 3 { expando } else { '$' };
    output.extend(std::iter::repeat_n(replacement, dollars));
    Cow::Owned(output)
}

fn ast_grep_expando(language: &str) -> char {
    match language.to_ascii_lowercase().as_str() {
        "c" | "cpp" => '𐀀',
        "c_sharp" | "csharp" | "css" | "elixir" | "go" | "haskell" | "hcl" | "kotlin" | "ocaml"
        | "php" | "python" | "ruby" | "rust" | "swift" => 'µ',
        "nix" => '_',
        _ => '$',
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PropertyCounts {
    pub named: u64,
    pub extra: u64,
    pub error: u64,
    pub missing: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NodeTypeCount {
    pub language: String,

    #[serde(rename = "type")]
    pub node_type: String,

    pub named: bool,
    pub count: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct NodeCounts {
    pub selected: u64,
    pub total: u64,
    pub by_property: PropertyCounts,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_type: Vec<NodeTypeCount>,
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
    count_source_with_filters(
        path,
        language,
        source,
        filter,
        &CompiledFilters::default(),
        false,
        parser,
    )
}

/// Measure source using a parser and precompiled selectors and exclusions.
///
/// # Errors
///
/// Returns an error when parsing is cancelled or a structural matcher cannot
/// process the source.
pub fn count_source_with_filters(
    path: &Path,
    language: &str,
    source: &[u8],
    filter: NodeFilter,
    filters: &CompiledFilters,
    collect_by_type: bool,
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
    let matched = filters.matched_node_ids(&tree, source)?;

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
    let mut excluded_root_depth = None;
    let mut by_type = BTreeMap::<(String, bool), u64>::new();
    let has_error = root.has_error();

    'walk: loop {
        let node = cursor.node();
        if excluded_root_depth.is_some_and(|excluded_depth| depth <= excluded_depth) {
            excluded_root_depth = None;
        }
        if excluded_root_depth.is_none() && matched.excluded.contains(&node.id()) {
            excluded_root_depth = Some(depth);
        }
        let is_named = node.is_named();
        let is_extra = node.is_extra();
        let is_error = has_error && node.is_error();
        let is_missing = has_error && node.is_missing();
        let traits = (u8::from(is_named) * NodeKind::Named.mask())
            | (u8::from(is_extra) * NodeKind::Extra.mask())
            | (u8::from(is_error) * NodeKind::Error.mask())
            | (u8::from(is_missing) * NodeKind::Missing.mask());
        let selected = excluded_root_depth.is_none()
            && filters.selects(node.kind(), node.id(), &matched)
            && filter.includes(NodeTraits(traits));
        selected_nodes += u64::from(selected);
        if collect_by_type && selected {
            *by_type
                .entry((node.kind().to_owned(), is_named))
                .or_default() += 1;
        }
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
                by_type: by_type
                    .into_iter()
                    .map(|((node_type, named), count)| NodeTypeCount {
                        language: language.to_owned(),
                        node_type,
                        named,
                        count,
                    })
                    .collect(),
            },
            max_depth,
            bytes: source.len() as u64,
        },
        elapsed: started.elapsed(),
    })
}

#[must_use]
pub fn report(files: Vec<FileMetrics>, filter: NodeFilter) -> Report {
    report_with_filter(files, filter.report())
}

#[must_use]
pub fn report_with_filter(mut files: Vec<FileMetrics>, filter: Filter) -> Report {
    files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    let mut totals = Totals::default();
    let mut by_type = BTreeMap::<(String, String, bool), u64>::new();
    for file in &files {
        totals.files += 1;
        totals.nodes.selected += file.nodes.selected;
        totals.nodes.total += file.nodes.total;
        totals.nodes.by_property.named += file.nodes.by_property.named;
        totals.nodes.by_property.extra += file.nodes.by_property.extra;
        totals.nodes.by_property.error += file.nodes.by_property.error;
        totals.nodes.by_property.missing += file.nodes.by_property.missing;
        totals.bytes += file.bytes;
        for node_type in &file.nodes.by_type {
            *by_type
                .entry((
                    node_type.language.clone(),
                    node_type.node_type.clone(),
                    node_type.named,
                ))
                .or_default() += node_type.count;
        }
    }
    totals.nodes.by_type = by_type
        .into_iter()
        .map(|((language, node_type, named), count)| NodeTypeCount {
            language,
            node_type,
            named,
            count,
        })
        .collect();
    Report {
        schema: REPORT_SCHEMA,
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        parser_backend: "tree-sitter-language-pack/1.15.8".to_owned(),
        filter,
        totals,
        files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_rust_with_filter(source: &str, filter: &Filter, collect_by_type: bool) -> FileMetrics {
        let grammar: Language = tree_sitter_rust::LANGUAGE.into();
        let compiled =
            CompiledFilters::compile("rust", &grammar, filter).expect("compile Rust filters");
        let mut parser = Parser::new();
        parser
            .set_language(&grammar)
            .expect("configure Rust parser");
        count_source_with_filters(
            Path::new("fixture.rs"),
            "rust",
            source.as_bytes(),
            filter.node_filter().expect("valid node filter"),
            &compiled,
            collect_by_type,
            &mut parser,
        )
        .expect("count Rust fixture")
        .metrics
    }

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
                by_type: Vec::new(),
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
                    by_type: Vec::new(),
                },
                max_depth: 4,
                bytes: 35,
            }],
            filter,
        );
        let json = serde_json::to_value(result).expect("serialize report");
        assert_eq!(json["schema"], REPORT_SCHEMA);
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
    fn selectors_are_or_combined_deduplicated_and_reported_by_type() {
        let source = "fn first() {}\nfn second() {}\nstruct Other;\n";
        let baseline = count_rust_with_filter(source, &Filter::default(), false);
        let filter = Filter {
            selected_types: vec![LanguageNodeType {
                language: "rust".to_owned(),
                node_type: "function_item".to_owned(),
            }],
            tree_sitter_selectors: vec![LanguageFilter {
                language: "rust".to_owned(),
                source: "(function_item) @select".to_owned(),
            }],
            ast_grep_selectors: vec![LanguageFilter {
                language: "rust".to_owned(),
                source: "fn first() { $$$BODY }".to_owned(),
            }],
            ast_grep_backend: Some(AST_GREP_BACKEND.to_owned()),
            ..Filter::default()
        };
        let selected = count_rust_with_filter(source, &filter, true);

        assert_eq!(selected.nodes.selected, 2);
        assert_eq!(selected.nodes.total, baseline.nodes.total);
        assert_eq!(selected.nodes.by_property, baseline.nodes.by_property);
        assert_eq!(
            selected.nodes.by_type,
            [NodeTypeCount {
                language: "rust".to_owned(),
                node_type: "function_item".to_owned(),
                named: true,
                count: 2,
            }]
        );
    }

    #[test]
    fn a_selector_for_another_language_selects_nothing() {
        let filter = Filter {
            selected_types: vec![LanguageNodeType {
                language: "javascript".to_owned(),
                node_type: "identifier".to_owned(),
            }],
            ..Filter::default()
        };
        let selected = count_rust_with_filter("fn main() {}\n", &filter, true);
        assert_eq!(selected.nodes.selected, 0);
        assert!(selected.nodes.by_type.is_empty());
        assert!(selected.nodes.total > 0);
    }

    #[test]
    fn exclusions_apply_after_selection_and_remove_captured_subtrees() {
        let filter = Filter {
            selected_types: vec![LanguageNodeType {
                language: "rust".to_owned(),
                node_type: "function_item".to_owned(),
            }],
            tree_sitter_queries: vec![LanguageFilter {
                language: "rust".to_owned(),
                source: r#"
((function_item
  name: (identifier) @_name) @exclude
 (#eq? @_name "skip"))
"#
                .trim()
                .to_owned(),
            }],
            ..Filter::default()
        };
        let selected = count_rust_with_filter("fn skip() {}\nfn keep() {}\n", &filter, false);
        assert_eq!(selected.nodes.selected, 1);
    }

    #[test]
    fn selector_queries_require_a_select_capture() {
        let grammar: Language = tree_sitter_rust::LANGUAGE.into();
        let filter = Filter {
            tree_sitter_selectors: vec![LanguageFilter {
                language: "rust".to_owned(),
                source: "(identifier) @wrong".to_owned(),
            }],
            ..Filter::default()
        };
        let error = CompiledFilters::compile("rust", &grammar, &filter)
            .err()
            .expect("missing @select should fail");
        assert!(error.to_string().contains("@select"));
    }

    #[test]
    fn exact_type_selectors_reject_unknown_types_for_their_language() {
        let grammar: Language = tree_sitter_rust::LANGUAGE.into();
        let filter = Filter {
            selected_types: vec![LanguageNodeType {
                language: "rust".to_owned(),
                node_type: "definitely_not_a_rust_node".to_owned(),
            }],
            ..Filter::default()
        };
        let error = CompiledFilters::compile("rust", &grammar, &filter)
            .err()
            .expect("unknown exact type should fail");
        assert!(error.to_string().contains("unknown Tree-sitter node type"));
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
                        by_type: Vec::new(),
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
