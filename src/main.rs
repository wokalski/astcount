use std::collections::{HashMap, HashSet, hash_map::Entry};
use std::fmt::Write as _;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
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
    let node_exclusions = args
        .exclude
        .iter()
        .copied()
        .map(NodeKind::from)
        .collect::<Vec<_>>();
    let filter = NodeFilter::excluding(&node_exclusions)?;
    let started = Instant::now();
    let builder = walk_builder(args)?;
    let current_dir = std::env::current_dir().context("get current directory")?;
    let saved_paths: HashSet<PathBuf> = args
        .save
        .iter()
        .map(|path| absolute_path(path, &current_dir))
        .collect();
    let (work, mut outcomes) = discover_files(&builder, &saved_paths, &current_dir);
    let stream_path_width = (args.stream && !args.json).then(|| {
        work.iter()
            .map(|item| item.path.display().to_string().chars().count())
            .chain(["path".len(), "total".len()])
            .max()
            .unwrap_or(5)
    });
    if let Some(path_width) = stream_path_width {
        print!("{}", render_human_header(&node_exclusions, path_width));
        std::io::stdout().flush().context("flush streamed output")?;
    }
    let mut stream_error = None;
    outcomes.extend(process_files(
        &work,
        args.language.as_deref(),
        filter,
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
            )
        );
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

fn process_files<F>(
    work: &[WorkItem],
    forced_language: Option<&str>,
    filter: NodeFilter,
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
                let mut parsers: HashMap<String, TsParser> = HashMap::new();
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
                        &mut parsers,
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
    let mut output = render_human_header(&report.filter.excluded, path_width);
    for file in files {
        output.push_str(&render_file_row(file, path_width));
    }
    output.push_str(&render_human_footer(
        report, elapsed, parse_time, path_width, show_stats,
    ));
    output
}

fn render_human_header(excluded: &[NodeKind], path_width: usize) -> String {
    let mut output = String::new();
    if !excluded.is_empty() {
        writeln!(
            output,
            "excluding: {}\n",
            excluded
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        )
        .expect("write to string");
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
    serde_json::json!({ "type": "file", "schema": 3, "file": file })
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
) -> String {
    let mut output = String::new();
    writeln!(
        output,
        "{:<path_width$}  {:>12}",
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
    fn human_output_is_path_first_size_sorted_and_stats_are_opt_in() {
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
        );
        assert!(with_stats.ends_with(
            "2 files · 1.0 MiB · 4.0 ms wall · 7.0 ms aggregate parse · 250.0 MiB/s · 0 error nodes · 0 missing nodes\n"
        ));
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
        assert_eq!(file["schema"], 3);
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
