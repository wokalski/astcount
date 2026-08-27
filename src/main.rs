use std::collections::{HashMap, HashSet, hash_map::Entry};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use astcount::{
    FileMetrics, NodeProperty, NodeSelection, Report, count_source_with_parser,
    detect_known_language, report,
};
use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use ignore::WalkBuilder;
use tree_sitter::Parser as TsParser;
use tree_sitter_language_pack::get_language;

#[derive(Clone, Copy, Debug, ValueEnum)]
enum NodePredicateArg {
    Named,
    Anonymous,
    Extra,
    Error,
    Missing,
}

impl NodePredicateArg {
    const fn property(self) -> Option<NodeProperty> {
        match self {
            Self::Named => Some(NodeProperty::Named),
            Self::Anonymous => None,
            Self::Extra => Some(NodeProperty::Extra),
            Self::Error => Some(NodeProperty::Error),
            Self::Missing => Some(NodeProperty::Missing),
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
    Count(CountArgs),
    /// Compare two saved JSON reports
    Compare(CompareArgs),
}

#[derive(Debug, ClapArgs)]
#[allow(clippy::struct_excessive_bools)]
struct CountArgs {
    /// Files or directories to measure
    #[arg(default_value = ".")]
    paths: Vec<PathBuf>,

    /// Require a Tree-sitter property (anonymous means not named)
    #[arg(long, value_enum, value_delimiter = ',')]
    require: Vec<NodePredicateArg>,

    /// Exclude a Tree-sitter property (anonymous means not named)
    #[arg(long, value_enum, value_delimiter = ',')]
    exclude: Vec<NodePredicateArg>,

    /// Force one Tree-sitter language for every input file
    #[arg(short, long)]
    language: Option<String>,

    /// Emit the complete report as JSON
    #[arg(long)]
    json: bool,

    /// Print one row per measured file
    #[arg(long)]
    files: bool,

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
    let mut required = Vec::new();
    let mut excluded_properties = Vec::new();
    for predicate in &args.require {
        match predicate.property() {
            Some(property) => required.push(property),
            None => excluded_properties.push(NodeProperty::Named),
        }
    }
    for predicate in &args.exclude {
        match predicate.property() {
            Some(property) => excluded_properties.push(property),
            None => required.push(NodeProperty::Named),
        }
    }
    let selection = NodeSelection::new(&required, &excluded_properties)?;
    let started = Instant::now();
    let builder = walk_builder(args)?;
    let current_dir = std::env::current_dir().context("get current directory")?;
    let excluded: HashSet<PathBuf> = args
        .save
        .iter()
        .map(|path| absolute_path(path, &current_dir))
        .collect();
    let (work, mut outcomes) = discover_files(&builder, &excluded, &current_dir);
    outcomes.extend(process_files(
        &work,
        args.language.as_deref(),
        selection,
        args.threads,
    ));

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

    let report = report(files, selection);
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

    if args.json {
        serde_json::to_writer_pretty(std::io::stdout().lock(), &report)?;
        println!();
    } else {
        print_human(&report, started.elapsed(), parse_time, args.files);
    }

    if args.fail_on_parse_error
        && (report.totals.error_nodes > 0 || report.totals.missing_nodes > 0)
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
    let percent = percent_change(before.totals.nodes, delta);
    if args.json {
        serde_json::to_writer_pretty(
            std::io::stdout().lock(),
            &serde_json::json!({
                "schema": 1,
                "parser_backend": after.parser_backend,
                "selection": after.selection,
                "before": { "path": args.before, "nodes": before.totals.nodes },
                "after": { "path": args.after, "nodes": after.totals.nodes },
                "delta_nodes": delta,
                "percent_change": percent,
            }),
        )?;
        println!();
    } else {
        println!("{:>12}  {}", before.totals.nodes, args.before.display());
        println!("{:>12}  {}", after.totals.nodes, args.after.display());
        print_change(delta, percent);
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

fn process_files(
    work: &[WorkItem],
    forced_language: Option<&str>,
    selection: NodeSelection,
    requested_workers: usize,
) -> Vec<Outcome> {
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

    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let next = &next;
            handles.push(scope.spawn(move || {
                let mut parsers: HashMap<String, TsParser> = HashMap::new();
                let mut source = Vec::new();
                let mut outcomes = Vec::new();
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = work.get(index) else {
                        break;
                    };
                    match process_path(
                        &item.path,
                        forced_language,
                        selection,
                        &mut parsers,
                        &mut source,
                    ) {
                        Ok(Some(result)) => {
                            outcomes.push(Outcome::Counted(result.metrics, result.elapsed));
                        }
                        Ok(None) => {}
                        Err(error) => {
                            outcomes.push(Outcome::Failed(item.path.clone(), error));
                        }
                    }
                }
                outcomes
            }));
        }

        handles
            .into_iter()
            .flat_map(|handle| {
                handle.join().unwrap_or_else(|_| {
                    vec![Outcome::Failed(
                        PathBuf::new(),
                        anyhow!("parser worker panicked"),
                    )]
                })
            })
            .collect()
    })
}

fn process_path(
    path: &Path,
    forced_language: Option<&str>,
    selection: NodeSelection,
    parsers: &mut HashMap<String, TsParser>,
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
    let parser = match parsers.entry(language.to_owned()) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) => {
            let grammar = get_language(language)?;
            let mut parser = TsParser::new();
            parser.set_language(&grammar)?;
            entry.insert(parser)
        }
    };
    count_source_with_parser(path, language, source, selection, parser).map(Some)
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
        report.schema == 2,
        "unsupported report schema {} in {}",
        report.schema,
        path.display()
    );
    Ok(report)
}

fn difference(current: &Report, previous: &Report) -> Result<i128> {
    ensure!(
        current.selection == previous.selection,
        "cannot compare reports with different node selections: {:?} versus {:?}",
        current.selection,
        previous.selection
    );
    ensure!(
        current.parser_backend == previous.parser_backend,
        "cannot compare parser backends {} and {}",
        current.parser_backend,
        previous.parser_backend
    );
    Ok(i128::from(current.totals.nodes) - i128::from(previous.totals.nodes))
}

#[allow(clippy::cast_precision_loss)]
fn print_human(report: &Report, elapsed: Duration, parse_time: Duration, show_files: bool) {
    if show_files {
        println!(
            "{:>12}  {:>12}  {:>8}  {:>8}  path",
            "nodes", "all", "error", "missing"
        );
        for file in &report.files {
            println!(
                "{:>12}  {:>12}  {:>8}  {:>8}  {} [{}]",
                file.nodes,
                file.total_nodes,
                file.error_nodes,
                file.missing_nodes,
                file.path.display(),
                file.language
            );
        }
    }
    println!(
        "{:>12}  {:>12}  {:>8}  {:>8}  total ({} files, {}, {:.1} MiB/s)",
        report.totals.nodes,
        report.totals.total_nodes,
        report.totals.error_nodes,
        report.totals.missing_nodes,
        report.totals.files,
        human_bytes(report.totals.bytes),
        throughput(report.totals.bytes, elapsed)
    );
    eprintln!(
        "measured in {:.1} ms wall time ({:.1} ms aggregate parse time)",
        elapsed.as_secs_f64() * 1_000.0,
        parse_time.as_secs_f64() * 1_000.0
    );
}

#[allow(clippy::cast_precision_loss)]
fn percent_change(before: u64, delta: i128) -> Option<f64> {
    (before != 0).then(|| delta as f64 * 100.0 / before as f64)
}

fn print_change(delta: i128, percent: Option<f64>) {
    match percent {
        Some(percent) => println!("change: {delta:+} nodes ({percent:+.2}%)"),
        None => println!("change: {delta:+} nodes (n/a)"),
    }
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
