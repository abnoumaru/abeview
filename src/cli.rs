use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::client;
use crate::git::Git;
use crate::target::{DiffSpec, Target};

/// Preview Markdown and git diffs in the browser, rendered like GitHub.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,

    /// Markdown file or directory to preview.
    path: Option<PathBuf>,

    /// Print the URL without opening a browser.
    #[arg(long, global = true)]
    no_open: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show a diff. Without SPEC, shows uncommitted changes including untracked files.
    ///
    /// SPEC can be a branch (compared from its merge base with --base or the
    /// default branch), A...B (from the merge base), or A..B (tree to tree).
    Diff {
        spec: Option<String>,
        /// Base for a single-branch SPEC. Defaults to origin/HEAD, main or master.
        #[arg(long)]
        base: Option<String>,
    },
    /// Show the changes introduced by a single commit.
    Show {
        #[arg(default_value = "HEAD")]
        rev: String,
    },
    /// Stop the background server.
    Stop,
    #[command(hide = true)]
    Serve,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let target = match (cli.command, cli.path) {
        (Some(Cmd::Serve), _) => return crate::server::run(),
        (Some(Cmd::Stop), _) => {
            if client::stop()? {
                println!("abeview server stopped");
            } else {
                println!("abeview server is not running");
            }
            return Ok(());
        }
        (Some(Cmd::Diff { spec, base }), _) => {
            let repo = current_repo()?;
            let git = Git::new(&repo);
            let spec = DiffSpec::parse(spec.as_deref(), base.as_deref(), || git.default_branch())?;
            diff_target(repo, spec)?
        }
        (Some(Cmd::Show { rev }), _) => diff_target(current_repo()?, DiffSpec::Commit { rev })?,
        (None, Some(path)) => {
            let path = path
                .canonicalize()
                .with_context(|| format!("{} does not exist", path.display()))?;
            if path.is_dir() {
                Target::MarkdownDir { path }
            } else {
                Target::MarkdownFile { path }
            }
        }
        (None, None) => {
            use clap::CommandFactory;
            Cli::command().print_help()?;
            return Ok(());
        }
    };

    let info = client::ensure_server()?;
    let url = client::open_view(&info, &target)?;
    println!("{url}");
    if !cli.no_open {
        open::that_detached(&url).context("failed to open the browser")?;
    }
    Ok(())
}

fn current_repo() -> Result<PathBuf> {
    let toplevel = Git::toplevel(&std::env::current_dir()?)?;
    Ok(toplevel.canonicalize()?)
}

/// Checks revisions here so mistakes show up in the terminal, not the browser.
fn diff_target(repo: PathBuf, spec: DiffSpec) -> Result<Target> {
    let git = Git::new(&repo);
    for rev in spec.revisions() {
        git.verify_commit(rev)?;
    }
    Ok(Target::Diff { repo, spec })
}
