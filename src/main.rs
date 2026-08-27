use std::collections::{HashMap, HashSet, hash_map::Entry};
use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use astcount::{
    FileMetrics, NodeFilter, NodeKind, Report, count_source_with_parser, detect_known_language,
    report,
};
use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
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

    /// Exclude node kinds or properties from the count
    #[arg(long, value_enum, value_delimiter = ',')]
    exclude: Vec<NodeKindArg>,

    /// Force one Tree-sitter language for every input file
    #[arg(short, long)]
    language: Option<String>,

    /// Emit the complete report as JSON
    #[arg(long)]
    json: bool,

    /// Print one row per measured file
    #[arg(long)]
    files: bool,

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
    let excluded = args
        .exclude
        .iter()
        .copied()
        .map(NodeKind::from)
        .collect::<Vec<_>>();
    let filter = NodeFilter::excluding(&excluded)?;
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
        filter,
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

    let report = report(files, filter);
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
        print_human(
            &report,
            started.elapsed(),
            parse_time,
            args.files,
            args.stats,
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
    filter: NodeFilter,
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
                        filter,
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
    filter: NodeFilter,
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
    count_source_with_parser(path, language, source, filter, parser).map(Some)
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
        report.schema == 3,
        "unsupported report schema {} in {}",
        report.schema,
        path.display()
    );
    NodeFilter::excluding(&report.filter.excluded)
        .with_context(|| format!("invalid node filter in {}", path.display()))?;
    Ok(report)
}

fn difference(current: &Report, previous: &Report) -> Result<i128> {
    ensure!(
        current.filter == previous.filter,
        "cannot compare reports with different node filters: {:?} versus {:?}",
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
) {
    print!(
        "{}",
        render_human(report, elapsed, parse_time, show_files, show_stats)
    );
}

#[allow(clippy::cast_precision_loss)]
fn render_human(
    report: &Report,
    elapsed: Duration,
    parse_time: Duration,
    show_files: bool,
    show_stats: bool,
) -> String {
    let mut output = String::new();
    let paths = report
        .files
        .iter()
        .filter(|_| show_files)
        .map(|file| file.path.display().to_string())
        .collect::<Vec<_>>();
    let path_width = paths
        .iter()
        .map(|path| path.chars().count())
        .chain(["path".len(), "total".len()])
        .max()
        .unwrap_or(5);
    if !report.filter.excluded.is_empty() {
        writeln!(
            output,
            "excluding: {}\n",
            report
                .filter
                .excluded
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        )
        .expect("write to string");
    }
    let node_width = 12;
    writeln!(output, "{:<path_width$}  {:>node_width$}", "path", "nodes").expect("write to string");
    if show_files {
        for (file, path) in report.files.iter().zip(paths) {
            writeln!(
                output,
                "{path:<path_width$}  {:>node_width$}",
                human_count(file.nodes.selected),
            )
            .expect("write to string");
        }
    }
    writeln!(
        output,
        "{:<path_width$}  {:>node_width$}",
        "total",
        human_count(report.totals.nodes.selected),
    )
    .expect("write to string");

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
    use astcount::{Filter, NodeCounts, PropertyCounts, Totals};

    fn sample_report(error: u64, missing: u64) -> Report {
        let files = vec![
            FileMetrics {
                path: PathBuf::from("src/lib.rs"),
                language: "rust".to_owned(),
                nodes: NodeCounts {
                    selected: 1_234,
                    total: 2_000,
                    by_property: PropertyCounts::default(),
                },
                max_depth: 4,
                bytes: 512 * 1024,
            },
            FileMetrics {
                path: PathBuf::from("src/main.rs"),
                language: "rust".to_owned(),
                nodes: NodeCounts {
                    selected: 98_765,
                    total: 100_000,
                    by_property: PropertyCounts::default(),
                },
                max_depth: 5,
                bytes: 512 * 1024,
            },
        ];
        Report {
            schema: 3,
            tool_version: "0.2.0".to_owned(),
            parser_backend: "test".to_owned(),
            filter: Filter {
                excluded: vec![NodeKind::Anonymous, NodeKind::Extra],
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
                },
            },
            files,
        }
    }

    #[test]
    fn human_output_is_path_first_and_stats_are_opt_in() {
        let report = sample_report(0, 0);
        let plain = render_human(
            &report,
            Duration::from_millis(4),
            Duration::from_millis(7),
            true,
            false,
        );
        let lines = plain.lines().collect::<Vec<_>>();
        assert_eq!(lines[0], "excluding: anonymous,extra");
        assert_eq!(
            lines[2].split_whitespace().collect::<Vec<_>>(),
            ["path", "nodes"]
        );
        assert!(lines[3].starts_with("src/lib.rs"));
        assert!(lines[3].ends_with("1,234"));
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
        );
        assert!(with_stats.ends_with(
            "2 files · 1.0 MiB · 4.0 ms wall · 7.0 ms aggregate parse · 250.0 MiB/s · 0 error nodes · 0 missing nodes\n"
        ));
    }

    #[test]
    fn raw_diagnostics_are_separate_from_the_filtered_count() {
        let output = render_human(
            &sample_report(3, 1),
            Duration::from_millis(4),
            Duration::from_millis(7),
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
}
