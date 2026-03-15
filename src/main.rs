use std::io::Write;
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};
use walkdir::WalkDir;

use pycg_rs::analyzer::CallGraph;
use pycg_rs::query::{
    self, MatchMode, QueryGraphMode, QueryRenderOptions, QueryResponse, TargetKind,
};
use pycg_rs::visgraph::{VisualGraph, VisualOptions};
use pycg_rs::writer::{self, JsonGraphMode, JsonOutputOptions};

#[derive(Parser)]
#[command(
    name = "pycg",
    about = "Generate call graphs for Python programs",
    subcommand_precedence_over_arg = true,
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Root directory for module name resolution
    #[arg(long, short = 'r', global = true)]
    root: Option<String>,

    /// Enable verbose logging
    #[arg(long, short = 'v', action = clap::ArgAction::Count, global = true)]
    verbose: u8,
}

#[derive(Subcommand)]
enum Command {
    Analyze(AnalyzeArgs),
    SymbolsIn(TargetQueryArgs),
    Summary(SummaryArgs),
    Callees(SymbolQueryArgs),
    Callers(SymbolQueryArgs),
    Neighbors(SymbolQueryArgs),
    Path(PathQueryArgs),
}

#[derive(Args, Clone)]
struct AnalyzeArgs {
    /// Python source files or directories to analyze
    #[arg(required = true)]
    files: Vec<PathBuf>,

    /// Output format
    #[arg(long, default_value = "dot")]
    format: Format,

    /// Draw defines edges
    #[arg(long, short = 'd')]
    defines: bool,

    /// Draw uses edges
    #[arg(long, short = 'u')]
    uses: bool,

    /// Color nodes by file
    #[arg(long, short = 'c')]
    colored: bool,

    /// Group nodes by namespace
    #[arg(long, short = 'g')]
    grouped: bool,

    /// Annotate nodes with file:line info
    #[arg(long, short = 'a')]
    annotated: bool,

    /// GraphViz rank direction
    #[arg(long, default_value = "TB")]
    rankdir: String,

    /// Show module-level import dependencies instead of symbol-level call graph
    #[arg(long, short = 'm')]
    modules: bool,
}

#[derive(Clone, clap::ValueEnum)]
enum Format {
    Dot,
    Tgf,
    Text,
    Json,
}

#[derive(Clone, clap::ValueEnum)]
enum QueryFormat {
    Text,
    Json,
}

#[derive(Clone, clap::ValueEnum)]
enum MatchModeArg {
    Exact,
    Suffix,
}

#[derive(Clone, clap::ValueEnum)]
enum TargetKindArg {
    Path,
    Module,
}

#[derive(Args, Clone)]
struct QueryCommonArgs {
    /// Output format
    #[arg(long, default_value = "json")]
    format: QueryFormat,
}

#[derive(Args, Clone)]
struct TargetQueryArgs {
    /// File, directory, or module target to query
    target: String,

    /// Python source files or directories to analyze
    #[arg(required = true)]
    files: Vec<PathBuf>,

    /// Interpret the target as a path or module name
    #[arg(long)]
    target_kind: Option<TargetKindArg>,

    /// Query the module graph rather than the symbol graph
    #[arg(long, short = 'm')]
    modules: bool,

    #[command(flatten)]
    common: QueryCommonArgs,
}

#[derive(Args, Clone)]
struct SummaryArgs {
    #[command(flatten)]
    target: TargetQueryArgs,

    /// Include per-symbol caller/callee counts
    #[arg(long)]
    stats: bool,
}

#[derive(Args, Clone)]
struct SymbolQueryArgs {
    /// Canonical symbol name to query
    symbol: String,

    /// Python source files or directories to analyze
    #[arg(required = true)]
    files: Vec<PathBuf>,

    /// Match mode for symbol lookup
    #[arg(long, default_value = "exact")]
    r#match: MatchModeArg,

    #[command(flatten)]
    common: QueryCommonArgs,
}

#[derive(Args, Clone)]
struct PathQueryArgs {
    /// Source canonical symbol name
    source: String,

    /// Target canonical symbol name
    target: String,

    /// Python source files or directories to analyze
    #[arg(required = true)]
    files: Vec<PathBuf>,

    /// Match mode for symbol lookup
    #[arg(long, default_value = "exact")]
    r#match: MatchModeArg,

    #[command(flatten)]
    common: QueryCommonArgs,
}

fn collect_python_files(paths: &[PathBuf]) -> Vec<String> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            for entry in WalkDir::new(path)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path().extension().is_some_and(|ext| ext == "py")
                        && !e.path().to_string_lossy().contains("__pycache__")
                })
            {
                files.push(entry.path().to_string_lossy().to_string());
            }
        } else if path.extension().is_some_and(|ext| ext == "py") {
            files.push(path.to_string_lossy().to_string());
        }
    }
    files.sort();
    files.dedup();
    files
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let log_level = match cli.verbose {
        0 => log::LevelFilter::Warn,
        1 => log::LevelFilter::Info,
        _ => log::LevelFilter::Debug,
    };
    env_logger::Builder::new().filter_level(log_level).init();

    let (output, should_fail) = match &cli.command {
        Command::Analyze(args) => (run_analyze(args, cli.root.as_deref())?, false),
        Command::SymbolsIn(args) => {
            run_target_query(&args.files, cli.root.as_deref(), |mut cg, json_inputs| {
                let response = query::symbols_in(
                    &mut cg,
                    &args.target,
                    infer_target_kind(&args.target, args.target_kind.as_ref()),
                    if args.modules {
                        QueryGraphMode::Module
                    } else {
                        QueryGraphMode::Symbol
                    },
                    &QueryRenderOptions {
                        analysis_root: cli.root.as_deref(),
                        inputs: &json_inputs,
                    },
                );
                Ok((
                    render_query_response(&response, &args.common.format),
                    response.is_error(),
                ))
            })?
        }
        Command::Summary(args) => run_target_query(
            &args.target.files,
            cli.root.as_deref(),
            |mut cg, json_inputs| {
                let response = query::summary(
                    &mut cg,
                    &args.target.target,
                    infer_target_kind(&args.target.target, args.target.target_kind.as_ref()),
                    if args.target.modules {
                        QueryGraphMode::Module
                    } else {
                        QueryGraphMode::Symbol
                    },
                    &QueryRenderOptions {
                        analysis_root: cli.root.as_deref(),
                        inputs: &json_inputs,
                    },
                    args.stats,
                );
                Ok((
                    render_query_response(&response, &args.target.common.format),
                    response.is_error(),
                ))
            },
        )?,
        Command::Callees(args) => {
            run_target_query(&args.files, cli.root.as_deref(), |mut cg, json_inputs| {
                let response = query::callees(
                    &mut cg,
                    &args.symbol,
                    to_match_mode(&args.r#match),
                    &QueryRenderOptions {
                        analysis_root: cli.root.as_deref(),
                        inputs: &json_inputs,
                    },
                );
                Ok((
                    render_query_response(&response, &args.common.format),
                    response.is_error(),
                ))
            })?
        }
        Command::Callers(args) => {
            run_target_query(&args.files, cli.root.as_deref(), |mut cg, json_inputs| {
                let response = query::callers(
                    &mut cg,
                    &args.symbol,
                    to_match_mode(&args.r#match),
                    &QueryRenderOptions {
                        analysis_root: cli.root.as_deref(),
                        inputs: &json_inputs,
                    },
                );
                Ok((
                    render_query_response(&response, &args.common.format),
                    response.is_error(),
                ))
            })?
        }
        Command::Neighbors(args) => {
            run_target_query(&args.files, cli.root.as_deref(), |mut cg, json_inputs| {
                let response = query::neighbors(
                    &mut cg,
                    &args.symbol,
                    to_match_mode(&args.r#match),
                    &QueryRenderOptions {
                        analysis_root: cli.root.as_deref(),
                        inputs: &json_inputs,
                    },
                );
                Ok((
                    render_query_response(&response, &args.common.format),
                    response.is_error(),
                ))
            })?
        }
        Command::Path(args) => {
            run_target_query(&args.files, cli.root.as_deref(), |mut cg, json_inputs| {
                let response = query::path(
                    &mut cg,
                    &args.source,
                    &args.target,
                    to_match_mode(&args.r#match),
                    &QueryRenderOptions {
                        analysis_root: cli.root.as_deref(),
                        inputs: &json_inputs,
                    },
                );
                Ok((
                    render_query_response(&response, &args.common.format),
                    response.is_error(),
                ))
            })?
        }
    };

    let mut stdout = std::io::stdout().lock();
    if let Err(e) = stdout.write_all(output.as_bytes())
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        return Err(e.into());
    }
    if should_fail {
        bail!("query failed");
    }
    Ok(())
}

fn run_target_query<F>(paths: &[PathBuf], root: Option<&str>, render: F) -> Result<(String, bool)>
where
    F: FnOnce(CallGraph, Vec<String>) -> Result<(String, bool)>,
{
    let files = collect_python_files(paths);
    if files.is_empty() {
        bail!("No Python files found");
    }
    let json_inputs: Vec<String> = paths
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect();
    eprintln!("Analyzing {} Python files...", files.len());
    let cg = CallGraph::new(&files, root)?;
    render(cg, json_inputs)
}

fn run_analyze(args: &AnalyzeArgs, root: Option<&str>) -> Result<String> {
    let files = collect_python_files(&args.files);
    if files.is_empty() {
        bail!("No Python files found");
    }
    let json_inputs: Vec<String> = args
        .files
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect();

    let (draw_defines, draw_uses) = if !args.defines && !args.uses {
        (false, true)
    } else {
        (args.defines, args.uses)
    };

    eprintln!("Analyzing {} Python files...", files.len());
    let mut cg = CallGraph::new(&files, root)?;

    let options = VisualOptions {
        draw_defines,
        draw_uses,
        colored: args.colored,
        grouped: args.grouped,
        annotated: args.annotated,
    };

    let output = if matches!(args.format, Format::Json) {
        // JSON bypasses the visual graph — serialize raw call graph data.
        if args.modules {
            let (mod_nodes, mod_uses, mod_defined) = cg.derive_module_graph();
            writer::write_json(
                &mod_nodes,
                &mod_defined,
                &pycg_rs::FxHashMap::default(),
                &mod_uses,
                &cg.diagnostics,
                &JsonOutputOptions {
                    graph_mode: JsonGraphMode::Module,
                    analysis_root: root,
                    inputs: &json_inputs,
                },
                &cg.interner,
            )
        } else {
            writer::write_json(
                &cg.nodes_arena,
                &cg.defined,
                &cg.defines_edges,
                &cg.uses_edges,
                &cg.diagnostics,
                &JsonOutputOptions {
                    graph_mode: JsonGraphMode::Symbol,
                    analysis_root: root,
                    inputs: &json_inputs,
                },
                &cg.interner,
            )
        }
    } else {
        let vg = if args.modules {
            let (mod_nodes, mod_uses, mod_defined) = cg.derive_module_graph();
            let mod_options = VisualOptions {
                draw_defines: false,
                draw_uses: true,
                colored: options.colored,
                grouped: options.grouped,
                annotated: options.annotated,
            };
            VisualGraph::from_call_graph(
                &mod_nodes,
                &mod_defined,
                &pycg_rs::FxHashMap::default(),
                &mod_uses,
                &mod_options,
                &cg.interner,
            )
        } else {
            VisualGraph::from_call_graph(
                &cg.nodes_arena,
                &cg.defined,
                &cg.defines_edges,
                &cg.uses_edges,
                &options,
                &cg.interner,
            )
        };

        match args.format {
            Format::Dot => writer::write_dot(&vg, &[format!("rankdir={}", args.rankdir)]),
            Format::Tgf => writer::write_tgf(&vg),
            Format::Text => writer::write_text(&vg),
            Format::Json => unreachable!(),
        }
    };

    Ok(output)
}

fn to_match_mode(mode: &MatchModeArg) -> MatchMode {
    match mode {
        MatchModeArg::Exact => MatchMode::Exact,
        MatchModeArg::Suffix => MatchMode::Suffix,
    }
}

fn infer_target_kind(target: &str, explicit: Option<&TargetKindArg>) -> TargetKind {
    match explicit {
        Some(TargetKindArg::Path) => TargetKind::Path,
        Some(TargetKindArg::Module) => TargetKind::Module,
        None => {
            let path = PathBuf::from(target);
            if path.exists()
                || target.ends_with(".py")
                || target.contains('/')
                || target.contains('\\')
            {
                TargetKind::Path
            } else {
                TargetKind::Module
            }
        }
    }
}

fn render_query_response(response: &QueryResponse, format: &QueryFormat) -> String {
    match format {
        QueryFormat::Json => response.render_json(),
        QueryFormat::Text => response.render_text(),
    }
}
