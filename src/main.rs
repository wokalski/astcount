use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, ValueEnum};
use deslop::{
    CountMode, FileMetrics, Report, count_source_with_parser, detect_known_language, report,
};
use ignore::WalkBuilder;
use tree_sitter::Parser as TsParser;
use tree_sitter_language_pack::get_language;

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum ModeArg {
    #[default]
    Ast,
    Named,
    All,
}

impl From<ModeArg> for CountMode {
    fn from(value: ModeArg) -> Self {
        match value {
            ModeArg::Ast => Self::Ast,
            ModeArg::Named => Self::Named,
            ModeArg::All => Self::All,
        }
    }
}

#[derive(Debug, Parser)]
#[command(version, about)]
#[allow(clippy::struct_excessive_bools)]
struct Args {
    /// Files or directories to measure
    #[arg(default_value = ".")]
    paths: Vec<PathBuf>,

    /// Count named AST-like nodes or every concrete syntax node
    #[arg(long, value_enum, default_value_t)]
    mode: ModeArg,

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

    /// Compare totals against a report created by --save
    #[arg(long, value_name = "FILE")]
    compare: Option<PathBuf>,

    /// Exit unsuccessfully if node count increased from --compare
    #[arg(long, requires = "compare")]
    fail_on_increase: bool,

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
        eprintln!("deslop: {error:#}");
        std::process::exit(2);
    }
}

#[allow(clippy::too_many_lines)]
fn run() -> Result<()> {
    let args = Args::parse();
    let mode = args.mode.into();
    let started = Instant::now();
    let builder = walk_builder(&args)?;
    let current_dir = std::env::current_dir().context("get current directory")?;
    let excluded: HashSet<PathBuf> = args
        .save
        .iter()
        .chain(args.compare.iter())
        .map(|path| absolute_path(path, &current_dir))
        .collect();
    let (work, mut outcomes) = discover_files(&builder, &excluded, &current_dir);
    outcomes.extend(process_files(
        &work,
        args.language.as_deref(),
        mode,
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
        eprintln!("deslop: {prefix}{error:#}");
    }

    let report = report(files, mode);
    if report.files.is_empty() && !failures.is_empty() {
        bail!("no files could be measured");
    }
    if report.files.is_empty() {
        bail!("no supported source files found");
    }

    let comparison = args
        .compare
        .as_deref()
        .map(|path| {
            let old = load_report(path)?;
            difference(&report, &old)
        })
        .transpose()?;

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
            comparison,
            args.files,
        );
    }

    if args.fail_on_parse_error && report.totals.errors > 0 {
        std::process::exit(1);
    }
    if args.fail_on_increase && comparison.is_some_and(|delta| delta > 0) {
        std::process::exit(1);
    }
    if failures.is_empty() {
        Ok(())
    } else {
        bail!("{} input(s) failed", failures.len())
    }
}

fn walk_builder(args: &Args) -> Result<WalkBuilder> {
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
    mode: CountMode,
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
                    if let Some(outcome) =
                        process_path(&item.path, forced_language, mode, &mut parsers, &mut source)
                    {
                        outcomes.push(outcome);
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
    mode: CountMode,
    parsers: &mut HashMap<String, TsParser>,
    source: &mut Vec<u8>,
) -> Option<Outcome> {
    let language = match forced_language.or_else(|| detect_known_language(path, &[])) {
        Some(language) => language,
        None => match detect_shebang(path) {
            Ok(Some(language)) => language,
            Ok(None) => return None,
            Err(error) => return Some(Outcome::Failed(path.to_path_buf(), error.into())),
        },
    };
    source.clear();
    let source_read = std::fs::File::open(path).and_then(|mut file| file.read_to_end(source));
    if let Err(error) = source_read {
        return Some(Outcome::Failed(path.to_path_buf(), error.into()));
    }
    if !parsers.contains_key(language) {
        let grammar = match get_language(language) {
            Ok(grammar) => grammar,
            Err(error) => return Some(Outcome::Failed(path.to_path_buf(), error.into())),
        };
        let mut parser = TsParser::new();
        if let Err(error) = parser.set_language(&grammar) {
            return Some(Outcome::Failed(path.to_path_buf(), error.into()));
        }
        parsers.insert(language.to_owned(), parser);
    }
    let parser = parsers.get_mut(language)?;
    match count_source_with_parser(path, language, source, mode, parser) {
        Ok(result) => Some(Outcome::Counted(result.metrics, result.elapsed)),
        Err(error) => Some(Outcome::Failed(path.to_path_buf(), error)),
    }
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
    if report.schema != 1 {
        bail!(
            "unsupported report schema {} in {}",
            report.schema,
            path.display()
        );
    }
    Ok(report)
}

fn difference(current: &Report, previous: &Report) -> Result<i128> {
    if current.mode != previous.mode {
        bail!(
            "cannot compare {} nodes with {} nodes",
            current.mode,
            previous.mode
        );
    }
    if current.parser_backend != previous.parser_backend {
        bail!(
            "cannot compare parser backends {} and {}",
            current.parser_backend,
            previous.parser_backend
        );
    }
    Ok(i128::from(current.totals.nodes) - i128::from(previous.totals.nodes))
}

#[allow(clippy::cast_precision_loss)]
fn print_human(
    report: &Report,
    elapsed: Duration,
    parse_time: Duration,
    comparison: Option<i128>,
    show_files: bool,
) {
    if show_files {
        println!("{:>12}  {:>12}  {:>8}  path", "nodes", "all", "errors");
        for file in &report.files {
            println!(
                "{:>12}  {:>12}  {:>8}  {} [{}]",
                file.nodes,
                file.total_nodes,
                file.errors,
                file.path.display(),
                file.language
            );
        }
    }
    println!(
        "{:>12}  {:>12}  {:>8}  total ({} files, {}, {:.1} MiB/s)",
        report.totals.nodes,
        report.totals.total_nodes,
        report.totals.errors,
        report.totals.files,
        human_bytes(report.totals.bytes),
        throughput(report.totals.bytes, elapsed)
    );
    if let Some(delta) = comparison {
        let percent = if delta == 0 {
            0.0
        } else {
            let previous = i128::from(report.totals.nodes) - delta;
            if previous == 0 {
                f64::INFINITY.copysign(delta as f64)
            } else {
                delta as f64 * 100.0 / previous as f64
            }
        };
        println!("change: {delta:+} nodes ({percent:+.2}%)");
    }
    eprintln!(
        "measured in {:.1} ms wall time ({:.1} ms aggregate parse time)",
        elapsed.as_secs_f64() * 1_000.0,
        parse_time.as_secs_f64() * 1_000.0
    );
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
