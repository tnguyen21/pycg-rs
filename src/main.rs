use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Parser;
use walkdir::WalkDir;

use pycallgraph_rs::analyzer::CallGraph;
use pycallgraph_rs::visgraph::{VisualGraph, VisualOptions};
use pycallgraph_rs::writer;

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

    /// Enable verbose logging
    #[arg(long, short = 'v', action = clap::ArgAction::Count)]
    verbose: u8,
}

#[derive(Clone, clap::ValueEnum)]
enum Format {
    Dot,
    Tgf,
    Text,
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

    let vg = VisualGraph::from_call_graph(
        &cg.nodes_arena,
        &cg.defined,
        &cg.defines_edges,
        &cg.uses_edges,
        &options,
    );

    let output = match cli.format {
        Format::Dot => writer::write_dot(&vg, &[format!("rankdir={}", cli.rankdir)]),
        Format::Tgf => writer::write_tgf(&vg),
        Format::Text => writer::write_text(&vg),
    };

    print!("{output}");
    Ok(())
}
