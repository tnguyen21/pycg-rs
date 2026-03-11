use std::io::Write;
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Parser;
use walkdir::WalkDir;

use pycg_rs::analyzer::CallGraph;
use pycg_rs::visgraph::{VisualGraph, VisualOptions};
use pycg_rs::writer::{self, JsonGraphMode, JsonOutputOptions};

#[derive(Parser)]
#[command(name = "pycg", about = "Generate call graphs for Python programs")]
struct Cli {
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

    /// Root directory for module name resolution
    #[arg(long, short = 'r')]
    root: Option<String>,

    /// GraphViz rank direction
    #[arg(long, default_value = "TB")]
    rankdir: String,

    /// Show module-level import dependencies instead of symbol-level call graph
    #[arg(long, short = 'm')]
    modules: bool,

    /// Enable verbose logging
    #[arg(long, short = 'v', action = clap::ArgAction::Count)]
    verbose: u8,
}

#[derive(Clone, clap::ValueEnum)]
enum Format {
    Dot,
    Tgf,
    Text,
    Json,
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

    let files = collect_python_files(&cli.files);
    if files.is_empty() {
        bail!("No Python files found");
    }
    let json_inputs: Vec<String> = cli
        .files
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect();

    // Default: show uses edges if neither --defines nor --uses specified
    let (draw_defines, draw_uses) = if !cli.defines && !cli.uses {
        (false, true)
    } else {
        (cli.defines, cli.uses)
    };

    eprintln!("Analyzing {} Python files...", files.len());
    let cg = CallGraph::new(&files, cli.root.as_deref())?;

    let options = VisualOptions {
        draw_defines,
        draw_uses,
        colored: cli.colored,
        grouped: cli.grouped,
        annotated: cli.annotated,
    };

    let output = if matches!(cli.format, Format::Json) {
        // JSON bypasses the visual graph — serialize raw call graph data.
        if cli.modules {
            let (mod_nodes, mod_uses, mod_defined) = cg.derive_module_graph();
            writer::write_json(
                &mod_nodes,
                &mod_defined,
                &std::collections::HashMap::new(),
                &mod_uses,
                &cg.diagnostics,
                &JsonOutputOptions {
                    graph_mode: JsonGraphMode::Module,
                    analysis_root: cli.root.as_deref(),
                    inputs: &json_inputs,
                },
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
                    analysis_root: cli.root.as_deref(),
                    inputs: &json_inputs,
                },
            )
        }
    } else {
        let vg = if cli.modules {
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
                &std::collections::HashMap::new(),
                &mod_uses,
                &mod_options,
            )
        } else {
            VisualGraph::from_call_graph(
                &cg.nodes_arena,
                &cg.defined,
                &cg.defines_edges,
                &cg.uses_edges,
                &options,
            )
        };

        match cli.format {
            Format::Dot => writer::write_dot(&vg, &[format!("rankdir={}", cli.rankdir)]),
            Format::Tgf => writer::write_tgf(&vg),
            Format::Text => writer::write_text(&vg),
            Format::Json => unreachable!(),
        }
    };

    let mut stdout = std::io::stdout().lock();
    if let Err(e) = stdout.write_all(output.as_bytes())
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        return Err(e.into());
    }
    Ok(())
}
