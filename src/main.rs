use std::collections::{HashMap, HashSet, hash_map::Entry};
use std::fmt::Write as _;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use astcount::{
    AST_GREP_BACKEND, CompiledFilters, FileMetrics, Filter, LanguageFilter, LanguageNodeType,
    NodeFilter, NodeKind, NodeTypeCount, REPORT_SCHEMA, Report, count_source_with_filters,
    detect_known_language, report_with_filter,
};
use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use tree_sitter::Parser as TsParser;
use tree_sitter_language_pack::get_language;

#[derive(Clone, Copy, Debug, ValueEnum)]
enum NodeKindArg {
    Named,
    Anonymous,
    Extra,
    Error,
    Missing,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ExcludePresetArg {
    /// Exclude conventional module-level tests in Rust, OCaml, JavaScript, and TypeScript
    ModuleTests,
}

impl From<NodeKindArg> for NodeKind {
    fn from(value: NodeKindArg) -> Self {
        match value {
            NodeKindArg::Named => Self::Named,
            NodeKindArg::Anonymous => Self::Anonymous,
            NodeKindArg::Extra => Self::Extra,
            NodeKindArg::Error => Self::Error,
            NodeKindArg::Missing => Self::Missing,
        }
    }
}

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    count: CountArgs,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Count syntax-tree nodes
    Count(Box<CountArgs>),
    /// Compare two saved JSON reports
    Compare(CompareArgs),
}

#[derive(Debug, ClapArgs)]
#[allow(clippy::struct_excessive_bools)]
struct CountArgs {
    /// Files or directories to measure
    #[arg(default_value = ".")]
    paths: Vec<PathBuf>,

    /// Select nodes whose exact grammar type matches LANGUAGE=TYPE
    #[arg(long, value_name = "LANGUAGE=TYPE")]
    select_type: Vec<String>,

    /// Select nodes captured as @select by LANGUAGE=QUERY
    #[arg(long, value_name = "LANGUAGE=QUERY")]
    select_query: Vec<String>,

    /// Select nodes captured as @select by a LANGUAGE=FILE query
    #[arg(long, value_name = "LANGUAGE=FILE")]
    select_query_file: Vec<String>,

    /// Select nodes matching an ast-grep LANGUAGE=PATTERN
    #[arg(long, value_name = "LANGUAGE=PATTERN")]
    select_pattern: Vec<String>,

    /// Exclude node kinds or properties from the count
    #[arg(long, value_enum, value_delimiter = ',')]
    exclude_kind: Vec<NodeKindArg>,

    /// Exclude files matching a path glob, relative to the current directory
    #[arg(long, value_name = "GLOB")]
    exclude_file: Vec<String>,

    /// Exclude subtrees captured as @exclude by LANGUAGE=QUERY
    #[arg(long, value_name = "LANGUAGE=QUERY")]
    exclude_query: Vec<String>,

    /// Exclude subtrees captured as @exclude by a LANGUAGE=FILE query
    #[arg(long, value_name = "LANGUAGE=FILE")]
    exclude_query_file: Vec<String>,

    /// Exclude subtrees matching an ast-grep LANGUAGE=PATTERN
    #[arg(long, value_name = "LANGUAGE=PATTERN")]
    exclude_pattern: Vec<String>,

    /// Apply a versioned structural-exclusion preset
    #[arg(long, value_enum, value_delimiter = ',')]
    exclude_preset: Vec<ExcludePresetArg>,

    /// Force one Tree-sitter language for every input file
    #[arg(short, long)]
    language: Option<String>,

    /// Emit the complete report as JSON
    #[arg(long)]
    json: bool,

    /// Print one row per measured file
    #[arg(long)]
    files: bool,

    /// Include a histogram of selected grammar-specific node types
    #[arg(long)]
    by_type: bool,

    /// Stream file results as they complete (JSONL with --json)
    #[arg(long)]
    stream: bool,

    /// Print file, byte, diagnostic, and timing statistics
    #[arg(long, conflicts_with = "json")]
    stats: bool,

    /// Save the complete JSON report to this path
    #[arg(long, value_name = "FILE")]
    save: Option<PathBuf>,

    /// Exit unsuccessfully if Tree-sitter produced error or missing nodes
    #[arg(long)]
    fail_on_parse_error: bool,

    /// Include hidden files and directories
    #[arg(long)]
    hidden: bool,

    /// Do not respect .gitignore and other ignore files
    #[arg(long)]
    no_ignore: bool,

    /// Number of parser workers (0 chooses automatically)
    #[arg(short = 'j', long, default_value_t = 0)]
    threads: usize,
}

#[derive(Debug, ClapArgs)]
struct CompareArgs {
    /// Earlier JSON report created by count --save
    before: PathBuf,

    /// Later JSON report created by count --save
    after: PathBuf,

    /// Emit the comparison as JSON
    #[arg(long)]
    json: bool,

    /// Exit unsuccessfully if the selected node count increased
    #[arg(long)]
    fail_on_increase: bool,
}

#[derive(Debug)]
enum Outcome {
    Counted(FileMetrics, Duration),
    Failed(PathBuf, anyhow::Error),
}

#[derive(Debug)]
struct WorkItem {
    path: PathBuf,
    bytes: u64,
}

struct LanguageWorker {
    parser: TsParser,
    filters: CompiledFilters,
}

struct SelectorSources {
    types: Vec<LanguageNodeType>,
    queries: Vec<LanguageFilter>,
    patterns: Vec<LanguageFilter>,
}

struct FileExclusions {
    patterns: Vec<String>,
    matcher: GlobSet,
}

impl FileExclusions {
    fn compile(patterns: &[String]) -> Result<Self> {
        let mut normalized = patterns
            .iter()
            .map(|pattern| {
                let pattern = pattern.trim();
                if pattern.ends_with('/') {
                    format!("{pattern}**")
                } else {
                    pattern.to_owned()
                }
            })
            .collect::<Vec<_>>();
        ensure!(
            normalized.iter().all(|pattern| !pattern.is_empty()),
            "file exclusion globs cannot be empty"
        );
        ensure!(
            normalized
                .iter()
                .all(|pattern| !Path::new(pattern).is_absolute()),
            "file exclusion globs must be relative to the current directory"
        );
        normalized.sort_unstable();
        normalized.dedup();

        let mut builder = GlobSetBuilder::new();
        for pattern in &normalized {
            add_glob(&mut builder, pattern)?;
            if !pattern.contains('/') {
                add_glob(&mut builder, &format!("**/{pattern}"))?;
            }
        }
        Ok(Self {
            patterns: normalized,
            matcher: builder.build().context("build file exclusion globs")?,
        })
    }

    fn is_match(&self, path: &Path, current_dir: &Path) -> bool {
        let absolute = absolute_path(path, current_dir);
        let relative = absolute.strip_prefix(current_dir).unwrap_or(path);
        self.matcher.is_match(relative)
    }
}

fn add_glob(builder: &mut GlobSetBuilder, pattern: &str) -> Result<()> {
    let glob = GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .with_context(|| format!("invalid file exclusion glob {pattern:?}"))?;
    builder.add(glob);
    Ok(())
}

const RUST_MODULE_TESTS_QUERY: &str = r#"
((attribute_item) @_test_attribute
 .
 (mod_item) @exclude
 (#match? @_test_attribute "cfg\\s*\\([^]]*\\btest\\b"))
((attribute_item) @exclude
 .
 (mod_item) @_test_module
 (#match? @exclude "cfg\\s*\\([^]]*\\btest\\b"))
"#;

const OCAML_MODULE_TESTS_QUERY: &str = r#"
((value_definition
  (attribute_id) @_test_attribute) @exclude
 (#match? @_test_attribute "^%?(test|test_unit|expect_test)$"))
((module_definition
  (attribute_id) @_test_attribute) @exclude
 (#match? @_test_attribute "^%?(test|test_module)$"))
((item_extension
  (attribute_id) @_test_attribute) @exclude
 (#match? @_test_attribute "^%?(test|test_unit|test_module|expect_test)$"))
"#;

const JAVASCRIPT_INLINE_TESTS_QUERY: &str = r#"
((if_statement
  condition: (parenthesized_expression
    (member_expression
      object: (meta_property) @_test_meta
      property: (property_identifier) @_test_property))) @exclude
 (#eq? @_test_meta "import.meta")
 (#eq? @_test_property "vitest"))
"#;

fn resolve_filter(args: &CountArgs) -> Result<Filter> {
    let SelectorSources {
        types: selected_types,
        queries: tree_sitter_selectors,
        patterns: ast_grep_selectors,
    } = resolve_selectors(args)?;
    let mut excluded = args
        .exclude_kind
        .iter()
        .copied()
        .map(NodeKind::from)
        .collect::<Vec<_>>();
    excluded.sort_unstable_by_key(|kind| kind.as_str());
    excluded.dedup();
    NodeFilter::excluding(&excluded)?;

    let mut tree_sitter_queries = args
        .exclude_query
        .iter()
        .map(|value| parse_language_filter(value, "Tree-sitter query"))
        .collect::<Result<Vec<_>>>()?;
    for value in &args.exclude_query_file {
        let (language, file) = split_language_value(value, "Tree-sitter query file")?;
        let source = std::fs::read_to_string(&file)
            .with_context(|| format!("read Tree-sitter query file {file}"))?;
        tree_sitter_queries.push(LanguageFilter { language, source });
    }
    let mut ast_grep_patterns = args
        .exclude_pattern
        .iter()
        .map(|value| parse_language_filter(value, "ast-grep pattern"))
        .collect::<Result<Vec<_>>>()?;
    let mut presets = Vec::new();
    for preset in &args.exclude_preset {
        match preset {
            ExcludePresetArg::ModuleTests => {
                presets.push("module-tests@1".to_owned());
                tree_sitter_queries.extend([
                    LanguageFilter {
                        language: "rust".to_owned(),
                        source: RUST_MODULE_TESTS_QUERY.trim().to_owned(),
                    },
                    LanguageFilter {
                        language: "ocaml".to_owned(),
                        source: OCAML_MODULE_TESTS_QUERY.trim().to_owned(),
                    },
                    LanguageFilter {
                        language: "javascript".to_owned(),
                        source: JAVASCRIPT_INLINE_TESTS_QUERY.trim().to_owned(),
                    },
                    LanguageFilter {
                        language: "typescript".to_owned(),
                        source: JAVASCRIPT_INLINE_TESTS_QUERY.trim().to_owned(),
                    },
                    LanguageFilter {
                        language: "tsx".to_owned(),
                        source: JAVASCRIPT_INLINE_TESTS_QUERY.trim().to_owned(),
                    },
                ]);
            }
        }
    }

    tree_sitter_queries.sort_unstable();
    tree_sitter_queries.dedup();
    ast_grep_patterns.sort_unstable();
    ast_grep_patterns.dedup();
    presets.sort_unstable();
    presets.dedup();
    let file_exclusions = FileExclusions::compile(&args.exclude_file)?;

    Ok(Filter {
        selected_types,
        tree_sitter_selectors,
        ast_grep_selectors,
        excluded,
        excluded_files: file_exclusions.patterns,
        tree_sitter_queries,
        ast_grep_backend: (!ast_grep_patterns.is_empty() || !args.select_pattern.is_empty())
            .then(|| AST_GREP_BACKEND.to_owned()),
        ast_grep_patterns,
        presets,
    })
}

fn resolve_selectors(args: &CountArgs) -> Result<SelectorSources> {
    let mut types = args
        .select_type
        .iter()
        .map(|value| parse_language_node_type(value))
        .collect::<Result<Vec<_>>>()?;
    let mut queries = args
        .select_query
        .iter()
        .map(|value| parse_language_filter(value, "Tree-sitter selector query"))
        .collect::<Result<Vec<_>>>()?;
    for value in &args.select_query_file {
        let (language, file) = split_language_value(value, "Tree-sitter selector query file")?;
        let source = std::fs::read_to_string(&file)
            .with_context(|| format!("read Tree-sitter selector query file {file}"))?;
        queries.push(LanguageFilter { language, source });
    }
    let mut patterns = args
        .select_pattern
        .iter()
        .map(|value| parse_language_filter(value, "ast-grep selector pattern"))
        .collect::<Result<Vec<_>>>()?;
    types.sort_unstable();
    types.dedup();
    queries.sort_unstable();
    queries.dedup();
    patterns.sort_unstable();
    patterns.dedup();
    Ok(SelectorSources {
        types,
        queries,
        patterns,
    })
}

fn parse_language_node_type(value: &str) -> Result<LanguageNodeType> {
    let (language, node_type) = split_language_value(value, "node type selector")?;
    let node_type = node_type.trim().to_owned();
    ensure!(!node_type.is_empty(), "node type selector cannot be empty");
    Ok(LanguageNodeType {
        language,
        node_type,
    })
}

fn parse_language_filter(value: &str, description: &str) -> Result<LanguageFilter> {
    let (language, source) = split_language_value(value, description)?;
    Ok(LanguageFilter { language, source })
}

fn split_language_value(value: &str, description: &str) -> Result<(String, String)> {
    let (language, source) = value
        .split_once('=')
        .ok_or_else(|| anyhow!("{description} must use LANGUAGE=VALUE syntax"))?;
    let language = language.trim().to_ascii_lowercase();
    ensure!(
        !language.is_empty(),
        "{description} language cannot be empty"
    );
    ensure!(!source.is_empty(), "{description} value cannot be empty");
    Ok((language, source.to_owned()))
}

fn main() {
    if let Err(error) = run() {
        eprintln!("astcount: {error:#}");
        std::process::exit(2);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Some(Command::Count(args)) => run_count(args),
        Some(Command::Compare(args)) => run_compare(args),
        None => run_count(&cli.count),
    }
}

#[allow(clippy::too_many_lines)]
fn run_count(args: &CountArgs) -> Result<()> {
    let report_filter = resolve_filter(args)?;
    let node_filter = report_filter.node_filter()?;
    let file_exclusions = FileExclusions::compile(&report_filter.excluded_files)?;
    let started = Instant::now();
    let builder = walk_builder(args)?;
    let current_dir = std::env::current_dir().context("get current directory")?;
    let saved_paths: HashSet<PathBuf> = args
        .save
        .iter()
        .map(|path| absolute_path(path, &current_dir))
        .collect();
    let (work, mut outcomes) =
        discover_files(&builder, &saved_paths, &file_exclusions, &current_dir);
    let stream_path_width = (args.stream && !args.json).then(|| {
        work.iter()
            .map(|item| item.path.display().to_string().chars().count())
            .chain(["path".len(), "total".len()])
            .max()
            .unwrap_or(5)
    });
    if let Some(path_width) = stream_path_width {
        print!("{}", render_human_header(&report_filter, path_width));
        std::io::stdout().flush().context("flush streamed output")?;
    }
    let mut stream_error = None;
    outcomes.extend(process_files(
        &work,
        args.language.as_deref(),
        node_filter,
        &report_filter,
        args.by_type,
        args.threads,
        |outcome| {
            if args.stream
                && stream_error.is_none()
                && let Outcome::Counted(file, _) = outcome
            {
                stream_error = write_stream_file(file, args.json, stream_path_width).err();
            }
        },
    ));
    if let Some(error) = stream_error {
        return Err(error).context("write streamed output");
    }

    let mut files = Vec::new();
    let mut failures = Vec::new();
    let mut parse_time = Duration::ZERO;
    for outcome in outcomes {
        match outcome {
            Outcome::Counted(file, elapsed) => {
                files.push(file);
                parse_time += elapsed;
            }
            Outcome::Failed(path, error) => failures.push((path, error)),
        }
    }

    for (path, error) in &failures {
        let prefix = if path.as_os_str().is_empty() {
            String::new()
        } else {
            format!("{}: ", path.display())
        };
        eprintln!("astcount: {prefix}{error:#}");
    }

    let report = report_with_filter(files, report_filter);
    if report.files.is_empty() && !failures.is_empty() {
        bail!("no files could be measured");
    }
    if report.files.is_empty() {
        bail!("no supported source files found");
    }

    if let Some(path) = &args.save {
        let json = serde_json::to_vec_pretty(&report)?;
        std::fs::write(path, json).with_context(|| format!("write {}", path.display()))?;
    }

    if args.json && args.stream {
        write_jsonl_summary(&report)?;
    } else if args.json {
        serde_json::to_writer_pretty(std::io::stdout().lock(), &report)?;
        println!();
    } else if let Some(path_width) = stream_path_width {
        print!(
            "{}",
            render_human_footer(
                &report,
                started.elapsed(),
                parse_time,
                path_width,
                args.stats,
                args.by_type,
            )
        );
    } else {
        print_human(
            &report,
            started.elapsed(),
            parse_time,
            args.files,
            args.stats,
            args.by_type,
        );
    }

    if args.fail_on_parse_error
        && (report.totals.nodes.by_property.error > 0
            || report.totals.nodes.by_property.missing > 0)
    {
        std::process::exit(1);
    }
    if failures.is_empty() {
        Ok(())
    } else {
        bail!("{} input(s) failed", failures.len())
    }
}

fn run_compare(args: &CompareArgs) -> Result<()> {
    let before = load_report(&args.before)?;
    let after = load_report(&args.after)?;
    let delta = difference(&after, &before)?;
    let percent = percent_change(before.totals.nodes.selected, delta);
    if args.json {
        serde_json::to_writer_pretty(
            std::io::stdout().lock(),
            &serde_json::json!({
                "schema": 1,
                "parser_backend": after.parser_backend,
                "filter": after.filter,
                "before": { "path": args.before, "nodes": before.totals.nodes.selected },
                "after": { "path": args.after, "nodes": after.totals.nodes.selected },
                "delta_nodes": delta,
                "percent_change": percent,
            }),
        )?;
        println!();
    } else {
        print_comparison(args, &before, &after, delta, percent);
    }
    if args.fail_on_increase && delta > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn walk_builder(args: &CountArgs) -> Result<WalkBuilder> {
    let (first, rest) = args
        .paths
        .split_first()
        .ok_or_else(|| anyhow!("no input paths"))?;
    let mut builder = WalkBuilder::new(first);
    for path in rest {
        builder.add(path);
    }
    builder
        .hidden(!args.hidden)
        .ignore(!args.no_ignore)
        .git_ignore(!args.no_ignore)
        .git_global(!args.no_ignore)
        .git_exclude(!args.no_ignore)
        .require_git(false)
        .follow_links(false);
    Ok(builder)
}

fn discover_files(
    builder: &WalkBuilder,
    excluded: &HashSet<PathBuf>,
    file_exclusions: &FileExclusions,
    current_dir: &Path,
) -> (Vec<WorkItem>, Vec<Outcome>) {
    let mut work = Vec::new();
    let mut failures = Vec::new();
    for entry in builder.build() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                failures.push(Outcome::Failed(PathBuf::new(), anyhow!(error)));
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.into_path();
        if !excluded.is_empty() && excluded.contains(&absolute_path(&path, current_dir)) {
            continue;
        }
        if file_exclusions.is_match(&path, current_dir) {
            continue;
        }
        let bytes = path.metadata().map_or(0, |metadata| metadata.len());
        work.push(WorkItem { path, bytes });
    }
    work.sort_unstable_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.path.cmp(&right.path))
    });
    (work, failures)
}

fn process_files<F>(
    work: &[WorkItem],
    forced_language: Option<&str>,
    filter: NodeFilter,
    report_filter: &Filter,
    collect_by_type: bool,
    requested_workers: usize,
    mut on_outcome: F,
) -> Vec<Outcome>
where
    F: FnMut(&Outcome),
{
    if work.is_empty() {
        return Vec::new();
    }
    let automatic = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let workers = if requested_workers == 0 {
        automatic
    } else {
        requested_workers
    }
    .min(work.len());
    let next = AtomicUsize::new(0);
    let (sender, receiver) = mpsc::channel();

    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let next = &next;
            let sender = sender.clone();
            handles.push(scope.spawn(move || {
                let mut languages: HashMap<String, LanguageWorker> = HashMap::new();
                let mut source = Vec::new();
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = work.get(index) else {
                        break;
                    };
                    let outcome = match process_path(
                        &item.path,
                        forced_language,
                        filter,
                        report_filter,
                        collect_by_type,
                        &mut languages,
                        &mut source,
                    ) {
                        Ok(Some(result)) => Some(Outcome::Counted(result.metrics, result.elapsed)),
                        Ok(None) => None,
                        Err(error) => Some(Outcome::Failed(item.path.clone(), error)),
                    };
                    if outcome.is_some_and(|outcome| sender.send(outcome).is_err()) {
                        break;
                    }
                }
            }));
        }

        drop(sender);
        let mut outcomes = receiver
            .into_iter()
            .inspect(&mut on_outcome)
            .collect::<Vec<_>>();
        for handle in handles {
            if handle.join().is_err() {
                let outcome = Outcome::Failed(PathBuf::new(), anyhow!("parser worker panicked"));
                on_outcome(&outcome);
                outcomes.push(outcome);
            }
        }
        outcomes
    })
}

fn process_path(
    path: &Path,
    forced_language: Option<&str>,
    filter: NodeFilter,
    report_filter: &Filter,
    collect_by_type: bool,
    languages: &mut HashMap<String, LanguageWorker>,
    source: &mut Vec<u8>,
) -> Result<Option<astcount::TimedMetrics>> {
    let language = match forced_language.or_else(|| detect_known_language(path, &[])) {
        Some(language) => Some(language),
        None => detect_shebang(path)?,
    };
    let Some(language) = language else {
        return Ok(None);
    };
    source.clear();
    std::fs::File::open(path)?.read_to_end(source)?;
    let worker = match languages.entry(language.to_owned()) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) => {
            let grammar = get_language(language)?;
            let mut parser = TsParser::new();
            parser.set_language(&grammar)?;
            let filters = CompiledFilters::compile(language, &grammar, report_filter)?;
            entry.insert(LanguageWorker { parser, filters })
        }
    };
    count_source_with_filters(
        path,
        language,
        source,
        filter,
        &worker.filters,
        collect_by_type,
        &mut worker.parser,
    )
    .map(Some)
}

fn detect_shebang(path: &Path) -> std::io::Result<Option<&'static str>> {
    let mut file = std::fs::File::open(path)?;
    let mut prefix = [0_u8; 256];
    let read = file.read(&mut prefix)?;
    Ok(detect_known_language(Path::new(""), &prefix[..read]))
}

fn absolute_path(path: &Path, current_dir: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        current_dir.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component),
        }
    }
    normalized
}

fn load_report(path: &Path) -> Result<Report> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let report: Report =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    ensure!(
        matches!(report.schema, 3 | REPORT_SCHEMA),
        "unsupported report schema {} in {}",
        report.schema,
        path.display()
    );
    report
        .filter
        .node_filter()
        .with_context(|| format!("invalid node filter in {}", path.display()))?;
    Ok(report)
}

fn difference(current: &Report, previous: &Report) -> Result<i128> {
    ensure!(
        current.filter == previous.filter,
        "cannot compare reports with different filters: {:?} versus {:?}",
        current.filter,
        previous.filter
    );
    ensure!(
        current.parser_backend == previous.parser_backend,
        "cannot compare parser backends {} and {}",
        current.parser_backend,
        previous.parser_backend
    );
    Ok(i128::from(current.totals.nodes.selected) - i128::from(previous.totals.nodes.selected))
}

#[allow(clippy::cast_precision_loss)]
fn print_human(
    report: &Report,
    elapsed: Duration,
    parse_time: Duration,
    show_files: bool,
    show_stats: bool,
    show_by_type: bool,
) {
    print!(
        "{}",
        render_human(
            report,
            elapsed,
            parse_time,
            show_files,
            show_stats,
            show_by_type,
        )
    );
}

#[allow(clippy::cast_precision_loss)]
fn render_human(
    report: &Report,
    elapsed: Duration,
    parse_time: Duration,
    show_files: bool,
    show_stats: bool,
    show_by_type: bool,
) -> String {
    let mut files = if show_files {
        report.files.iter().collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    files.sort_unstable_by(|left, right| {
        left.nodes
            .selected
            .cmp(&right.nodes.selected)
            .then_with(|| left.path.cmp(&right.path))
    });
    let path_width = files
        .iter()
        .map(|file| file.path.display().to_string().chars().count())
        .chain(["path".len(), "total".len()])
        .max()
        .unwrap_or(5);
    let mut output = render_human_header(&report.filter, path_width);
    for file in files {
        output.push_str(&render_file_row(file, path_width));
    }
    output.push_str(&render_human_footer(
        report,
        elapsed,
        parse_time,
        path_width,
        show_stats,
        show_by_type,
    ));
    output
}

fn render_human_header(filter: &Filter, path_width: usize) -> String {
    let mut output = String::new();
    if !filter.selected_types.is_empty() {
        writeln!(
            output,
            "selecting types: {}",
            filter
                .selected_types
                .iter()
                .map(|selected| format!("{}={}", selected.language, selected.node_type))
                .collect::<Vec<_>>()
                .join(",")
        )
        .expect("write to string");
    }
    let mut selectors = Vec::new();
    if !filter.tree_sitter_selectors.is_empty() {
        selectors.push(format!(
            "{} Tree-sitter quer{}",
            filter.tree_sitter_selectors.len(),
            if filter.tree_sitter_selectors.len() == 1 {
                "y"
            } else {
                "ies"
            }
        ));
    }
    if !filter.ast_grep_selectors.is_empty() {
        selectors.push(format!(
            "{} ast-grep pattern{}",
            filter.ast_grep_selectors.len(),
            if filter.ast_grep_selectors.len() == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    if !selectors.is_empty() {
        writeln!(output, "selecting syntax: {}", selectors.join(", ")).expect("write to string");
    }
    if !filter.excluded.is_empty() {
        writeln!(
            output,
            "excluding kinds: {}",
            filter
                .excluded
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        )
        .expect("write to string");
    }
    if !filter.excluded_files.is_empty() {
        writeln!(
            output,
            "excluding files: {}",
            filter.excluded_files.join(",")
        )
        .expect("write to string");
    }
    if !filter.presets.is_empty() {
        writeln!(output, "excluding presets: {}", filter.presets.join(","))
            .expect("write to string");
    }
    let mut structural = Vec::new();
    if !filter.tree_sitter_queries.is_empty() {
        structural.push(format!(
            "{} Tree-sitter quer{}",
            filter.tree_sitter_queries.len(),
            if filter.tree_sitter_queries.len() == 1 {
                "y"
            } else {
                "ies"
            }
        ));
    }
    if !filter.ast_grep_patterns.is_empty() {
        structural.push(format!(
            "{} ast-grep pattern{}",
            filter.ast_grep_patterns.len(),
            if filter.ast_grep_patterns.len() == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    if !structural.is_empty() {
        writeln!(output, "excluding syntax: {}", structural.join(", ")).expect("write to string");
    }
    if !output.is_empty() {
        writeln!(output).expect("write to string");
    }
    let node_width = 12;
    writeln!(output, "{:<path_width$}  {:>node_width$}", "path", "nodes").expect("write to string");
    output
}

fn render_file_row(file: &FileMetrics, path_width: usize) -> String {
    let path = file.path.display().to_string();
    format!(
        "{path:<path_width$}  {:>12}\n",
        human_count(file.nodes.selected)
    )
}

fn write_stream_file(file: &FileMetrics, json: bool, path_width: Option<usize>) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    if json {
        serde_json::to_writer(&mut output, &jsonl_file_event(file))?;
        writeln!(output)?;
    } else {
        let path_width = path_width.expect("human streams have a path width");
        output.write_all(render_file_row(file, path_width).as_bytes())?;
    }
    output.flush()?;
    Ok(())
}

fn write_jsonl_summary(report: &Report) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer(&mut output, &jsonl_summary_event(report))?;
    writeln!(output)?;
    output.flush()?;
    Ok(())
}

fn jsonl_file_event(file: &FileMetrics) -> serde_json::Value {
    serde_json::json!({ "type": "file", "schema": REPORT_SCHEMA, "file": file })
}

fn jsonl_summary_event(report: &Report) -> serde_json::Value {
    serde_json::json!({
        "type": "summary",
        "schema": report.schema,
        "tool_version": &report.tool_version,
        "parser_backend": &report.parser_backend,
        "filter": &report.filter,
        "totals": &report.totals,
    })
}

#[allow(clippy::cast_precision_loss)]
fn render_human_footer(
    report: &Report,
    elapsed: Duration,
    parse_time: Duration,
    path_width: usize,
    show_stats: bool,
    show_by_type: bool,
) -> String {
    let mut output = String::new();
    writeln!(
        output,
        "{:<path_width$}  {:>12}",
        "total",
        human_count(report.totals.nodes.selected),
    )
    .expect("write to string");

    if show_by_type {
        output.push_str(&render_type_histogram(&report.totals.nodes.by_type));
    }

    let properties = &report.totals.nodes.by_property;
    if show_stats {
        writeln!(output).expect("write to string");
        writeln!(
            output,
            "{} files · {} · {:.1} ms wall · {:.1} ms aggregate parse · {:.1} MiB/s · {} error nodes · {} missing nodes",
            human_count(report.totals.files),
            human_bytes(report.totals.bytes),
            elapsed.as_secs_f64() * 1_000.0,
            parse_time.as_secs_f64() * 1_000.0,
            throughput(report.totals.bytes, elapsed),
            human_count(properties.error),
            human_count(properties.missing),
        )
        .expect("write to string");
    } else if properties.error > 0 || properties.missing > 0 {
        writeln!(output).expect("write to string");
        writeln!(
            output,
            "parser diagnostics (unfiltered): {} error nodes · {} missing nodes",
            human_count(properties.error),
            human_count(properties.missing)
        )
        .expect("write to string");
    }
    output
}

fn render_type_histogram(node_types: &[NodeTypeCount]) -> String {
    let mut output = String::new();
    writeln!(output).expect("write to string");
    if node_types.is_empty() {
        writeln!(output, "node types (selected): none").expect("write to string");
        return output;
    }

    let mut node_types = node_types.iter().collect::<Vec<_>>();
    node_types.sort_unstable_by(|left, right| {
        left.count
            .cmp(&right.count)
            .then_with(|| left.language.cmp(&right.language))
            .then_with(|| left.node_type.cmp(&right.node_type))
            .then_with(|| left.named.cmp(&right.named))
    });
    let language_width = node_types
        .iter()
        .map(|entry| entry.language.chars().count())
        .chain(["language".len()])
        .max()
        .unwrap_or("language".len());
    let type_width = node_types
        .iter()
        .map(|entry| entry.node_type.chars().count())
        .chain(["type".len()])
        .max()
        .unwrap_or("type".len());

    writeln!(output, "node types (selected)").expect("write to string");
    writeln!(
        output,
        "{:<language_width$}  {:<type_width$}  {:<9}  {:>12}",
        "language", "type", "kind", "nodes"
    )
    .expect("write to string");
    for entry in node_types {
        writeln!(
            output,
            "{:<language_width$}  {:<type_width$}  {:<9}  {:>12}",
            entry.language,
            entry.node_type,
            if entry.named { "named" } else { "anonymous" },
            human_count(entry.count)
        )
        .expect("write to string");
    }
    output
}

#[allow(clippy::cast_precision_loss)]
fn percent_change(before: u64, delta: i128) -> Option<f64> {
    (before != 0).then(|| delta as f64 * 100.0 / before as f64)
}

fn print_comparison(
    args: &CompareArgs,
    before: &Report,
    after: &Report,
    delta: i128,
    percent: Option<f64>,
) {
    let before_path = args.before.display().to_string();
    let after_path = args.after.display().to_string();
    let path_width = before_path
        .chars()
        .count()
        .max(after_path.chars().count())
        .max("path".len())
        .max("change".len());
    println!("{:<path_width$}  {:>12}", "path", "nodes");
    println!(
        "{before_path:<path_width$}  {:>12}",
        human_count(before.totals.nodes.selected)
    );
    println!(
        "{after_path:<path_width$}  {:>12}",
        human_count(after.totals.nodes.selected)
    );
    let change = match percent {
        Some(percent) => format!("{delta:+} ({percent:+.2}%)"),
        None => format!("{delta:+} (n/a)"),
    };
    println!("{:<path_width$}  {:>12}", "change", change);
}

#[allow(clippy::cast_precision_loss)]
fn throughput(bytes: u64, elapsed: Duration) -> f64 {
    bytes as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64()
}

#[allow(clippy::cast_precision_loss)]
fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn human_count(value: u64) -> String {
    let digits = value.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(digit);
    }
    formatted
}

#[cfg(test)]
mod tests {
    use super::*;
    use astcount::{NodeCounts, PropertyCounts, Totals};

    fn selected_with_filter(
        language: &str,
        grammar: &tree_sitter::Language,
        source: &str,
        filter: &Filter,
    ) -> u64 {
        let filters =
            CompiledFilters::compile(language, grammar, filter).expect("compile syntax filters");
        let mut parser = TsParser::new();
        parser.set_language(grammar).expect("configure parser");
        count_source_with_filters(
            Path::new("fixture"),
            language,
            source.as_bytes(),
            NodeFilter::excluding(&[NodeKind::Anonymous]).unwrap(),
            &filters,
            false,
            &mut parser,
        )
        .expect("count fixture")
        .metrics
        .nodes
        .selected
    }

    fn query_filter(language: &str, source: &str) -> Filter {
        Filter {
            excluded: vec![NodeKind::Anonymous],
            tree_sitter_queries: vec![LanguageFilter {
                language: language.to_owned(),
                source: source.trim().to_owned(),
            }],
            ..Filter::default()
        }
    }

    fn sample_report(error: u64, missing: u64) -> Report {
        let files = vec![
            FileMetrics {
                path: PathBuf::from("src/main.rs"),
                language: "rust".to_owned(),
                nodes: NodeCounts {
                    selected: 98_765,
                    total: 100_000,
                    by_property: PropertyCounts::default(),
                    by_type: Vec::new(),
                },
                max_depth: 5,
                bytes: 512 * 1024,
            },
            FileMetrics {
                path: PathBuf::from("src/lib.rs"),
                language: "rust".to_owned(),
                nodes: NodeCounts {
                    selected: 1_234,
                    total: 2_000,
                    by_property: PropertyCounts::default(),
                    by_type: Vec::new(),
                },
                max_depth: 4,
                bytes: 512 * 1024,
            },
        ];
        Report {
            schema: REPORT_SCHEMA,
            tool_version: env!("CARGO_PKG_VERSION").to_owned(),
            parser_backend: "test".to_owned(),
            filter: Filter {
                excluded: vec![NodeKind::Anonymous, NodeKind::Extra],
                ..Filter::default()
            },
            totals: Totals {
                files: 2,
                bytes: 1024 * 1024,
                nodes: NodeCounts {
                    selected: 99_999,
                    total: 102_000,
                    by_property: PropertyCounts {
                        error,
                        missing,
                        ..PropertyCounts::default()
                    },
                    by_type: Vec::new(),
                },
            },
            files,
        }
    }

    #[test]
    fn human_output_is_path_first_size_sorted_and_stats_are_opt_in() {
        let report = sample_report(0, 0);
        let plain = render_human(
            &report,
            Duration::from_millis(4),
            Duration::from_millis(7),
            true,
            false,
            false,
        );
        let lines = plain.lines().collect::<Vec<_>>();
        assert_eq!(lines[0], "excluding kinds: anonymous,extra");
        assert_eq!(
            lines[2].split_whitespace().collect::<Vec<_>>(),
            ["path", "nodes"]
        );
        assert!(lines[3].starts_with("src/lib.rs"));
        assert!(lines[3].ends_with("1,234"));
        assert!(lines[4].starts_with("src/main.rs"));
        assert!(lines[4].ends_with("98,765"));
        assert!(lines[5].starts_with("total"));
        assert!(lines[5].ends_with("99,999"));
        assert!(!plain.contains("files ·"));
        assert!(!plain.contains("wall"));

        let with_stats = render_human(
            &report,
            Duration::from_millis(4),
            Duration::from_millis(7),
            true,
            true,
            false,
        );
        assert!(with_stats.ends_with(
            "2 files · 1.0 MiB · 4.0 ms wall · 7.0 ms aggregate parse · 250.0 MiB/s · 0 error nodes · 0 missing nodes\n"
        ));
    }

    #[test]
    fn human_type_histogram_is_selected_and_largest_last() {
        let mut report = sample_report(0, 0);
        report.totals.nodes.by_type = vec![
            NodeTypeCount {
                language: "rust".to_owned(),
                node_type: "identifier".to_owned(),
                named: true,
                count: 10,
            },
            NodeTypeCount {
                language: "rust".to_owned(),
                node_type: ";".to_owned(),
                named: false,
                count: 2,
            },
        ];
        let output = render_human(
            &report,
            Duration::from_millis(4),
            Duration::from_millis(7),
            false,
            false,
            true,
        );
        assert!(output.contains("node types (selected)\n"));
        assert!(output.contains("rust      ;           anonymous"));
        assert!(output.contains("rust      identifier  named"));
        assert!(
            output.find("anonymous").expect("anonymous row")
                < output.find("identifier").expect("identifier row")
        );
    }

    #[test]
    fn selector_flags_resolve_to_comparable_report_metadata() {
        let cli = Cli::try_parse_from([
            "astcount",
            "count",
            ".",
            "--select-type",
            "RUST=function_item",
            "--select-query",
            "rust=(identifier) @select",
            "--select-pattern",
            "rust=fn $NAME() { $$$BODY }",
        ])
        .expect("parse selector flags");
        let Command::Count(args) = cli.command.expect("explicit count command") else {
            panic!("expected count command");
        };
        let filter = resolve_filter(&args).expect("resolve selectors");
        assert_eq!(filter.selected_types[0].language, "rust");
        assert_eq!(filter.selected_types[0].node_type, "function_item");
        assert_eq!(filter.tree_sitter_selectors.len(), 1);
        assert_eq!(filter.ast_grep_selectors.len(), 1);
        assert_eq!(filter.ast_grep_backend.as_deref(), Some(AST_GREP_BACKEND));
    }

    #[test]
    fn streamed_rows_preserve_completion_order_and_jsonl_events_are_typed() {
        let report = sample_report(0, 0);
        let streamed = report
            .files
            .iter()
            .map(|file| render_file_row(file, 12))
            .collect::<String>();
        assert!(
            streamed.find("src/main.rs").expect("first completed row")
                < streamed.find("src/lib.rs").expect("second completed row")
        );

        let file_line = serde_json::to_string(&jsonl_file_event(&report.files[0]))
            .expect("serialize file event");
        let file: serde_json::Value = serde_json::from_str(&file_line).expect("parse file event");
        assert_eq!(file["type"], "file");
        assert_eq!(file["schema"], REPORT_SCHEMA);
        assert_eq!(file["file"]["path"], "src/main.rs");

        let summary_line =
            serde_json::to_string(&jsonl_summary_event(&report)).expect("serialize summary event");
        let summary: serde_json::Value =
            serde_json::from_str(&summary_line).expect("parse summary event");
        assert_eq!(summary["type"], "summary");
        assert_eq!(summary["totals"]["files"], 2);
        assert!(summary.get("files").is_none());
    }

    #[test]
    fn raw_diagnostics_are_separate_from_the_filtered_count() {
        let output = render_human(
            &sample_report(3, 1),
            Duration::from_millis(4),
            Duration::from_millis(7),
            false,
            false,
            false,
        );
        let total = output
            .lines()
            .find(|line| line.starts_with("total"))
            .expect("total row");
        assert_eq!(
            total.split_whitespace().collect::<Vec<_>>(),
            ["total", "99,999"]
        );
        assert!(
            output.ends_with("parser diagnostics (unfiltered): 3 error nodes · 1 missing nodes\n")
        );
    }

    #[test]
    fn module_test_preset_excludes_rust_ocaml_and_js_ts_inline_tests() {
        let cases: [(&str, tree_sitter::Language, &str, &str, &str); 5] = [
            (
                "rust",
                tree_sitter_rust::LANGUAGE.into(),
                "fn production() {}\n",
                "fn production() {}\n#[cfg(test)]\nmod tests { #[test] fn works() {} }\n",
                RUST_MODULE_TESTS_QUERY,
            ),
            (
                "ocaml",
                tree_sitter_ocaml::LANGUAGE_OCAML.into(),
                "let production = 1\n",
                "let production = 1\nlet%test \"works\" = true\nmodule%test Tests = struct let x = 1 end\n",
                OCAML_MODULE_TESTS_QUERY,
            ),
            (
                "javascript",
                tree_sitter_javascript::LANGUAGE.into(),
                "const production = 1;\n",
                "const production = 1;\nif (import.meta.vitest) { const { it } = import.meta.vitest; it('inline', () => {}); }\n",
                JAVASCRIPT_INLINE_TESTS_QUERY,
            ),
            (
                "typescript",
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                "export function production(value: number): number { return value + 1; }\n",
                "export function production(value: number): number { return value + 1; }\nif (import.meta.vitest) { const { it } = import.meta.vitest; it('inline', () => {}); }\n",
                JAVASCRIPT_INLINE_TESTS_QUERY,
            ),
            (
                "tsx",
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                "export const Production = () => <div />;\n",
                "export const Production = () => <div />;\nif (import.meta.vitest) { const { it } = import.meta.vitest; it('inline', () => {}); }\n",
                JAVASCRIPT_INLINE_TESTS_QUERY,
            ),
        ];

        for (language, grammar, production, with_tests, query) in cases {
            let filter = query_filter(language, query);
            assert_eq!(
                selected_with_filter(language, &grammar, with_tests, &filter),
                selected_with_filter(language, &grammar, production, &Filter::default()),
                "{language} preset should remove complete module-level tests"
            );
        }
    }

    #[test]
    fn module_test_preset_keeps_ordinary_javascript_test_calls() {
        let grammar: tree_sitter::Language = tree_sitter_javascript::LANGUAGE.into();
        let source = "describe('suite', () => { test('works', () => {}); });\n";
        let filter = query_filter("javascript", JAVASCRIPT_INLINE_TESTS_QUERY);
        assert_eq!(
            selected_with_filter("javascript", &grammar, source, &filter),
            selected_with_filter("javascript", &grammar, source, &Filter::default()),
        );
    }

    #[test]
    fn ast_grep_patterns_use_metavariables_with_existing_language_trees() {
        let cases: [(&str, tree_sitter::Language, &str, &str, &str); 3] = [
            (
                "rust",
                tree_sitter_rust::LANGUAGE.into(),
                "fn keep() {}\n",
                "fn keep() {}\nfn removable() { let x = 1; }\n",
                "fn removable() { $$$BODY }",
            ),
            (
                "ocaml",
                tree_sitter_ocaml::LANGUAGE_OCAML.into(),
                "let keep = 1\n",
                "let keep = 1\nlet removable = 2\n",
                "let removable = $VALUE",
            ),
            (
                "javascript",
                tree_sitter_javascript::LANGUAGE.into(),
                "const keep = 1;\n",
                "const keep = 1;\nconst removable = 2;\n",
                "const removable = $VALUE;",
            ),
        ];

        for (language, grammar, production, with_match, pattern) in cases {
            let filter = Filter {
                excluded: vec![NodeKind::Anonymous],
                ast_grep_patterns: vec![LanguageFilter {
                    language: language.to_owned(),
                    source: pattern.to_owned(),
                }],
                ast_grep_backend: Some(AST_GREP_BACKEND.to_owned()),
                ..Filter::default()
            };
            assert_eq!(
                selected_with_filter(language, &grammar, with_match, &filter),
                selected_with_filter(language, &grammar, production, &Filter::default()),
                "{language} ast-grep pattern should remove the complete match"
            );
        }
    }

    #[test]
    fn file_exclusion_globs_match_basenames_and_relative_trees() {
        let cwd = Path::new("/repo");
        let exclusions = FileExclusions::compile(&["*.test.rs".to_owned(), "tests/".to_owned()])
            .expect("compile globs");
        assert!(exclusions.is_match(Path::new("/repo/src/parser.test.rs"), cwd));
        assert!(exclusions.is_match(Path::new("/repo/tests/unit/parser.rs"), cwd));
        assert!(!exclusions.is_match(Path::new("/repo/src/parser.rs"), cwd));
        assert_eq!(exclusions.patterns, ["*.test.rs", "tests/**"]);
    }

    #[test]
    fn comparison_rejects_different_structural_filters() {
        let before = sample_report(0, 0);
        let mut after = before.clone();
        after.filter.excluded_files.push("tests/**".to_owned());
        let error = difference(&after, &before).expect_err("filters must match");
        assert!(error.to_string().contains("different filters"));
    }
}
