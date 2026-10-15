use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{broadcast, mpsc};

use crate::git::Git;
use crate::target::Target;

const DEBOUNCE: Duration = Duration::from_millis(250);

/// Watches whatever `target` depends on and sends on `changed` when the
/// rendered page may be out of date. Dropping the watcher stops it.
pub fn spawn(target: &Target, changed: broadcast::Sender<()>) -> Result<RecommendedWatcher> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<PathBuf>>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            let _ = tx.send(event.paths);
        }
    })?;

    let (watch_path, mode) = match target {
        Target::MarkdownFile { path } => (
            path.parent().context("file has no parent")?.to_path_buf(),
            RecursiveMode::NonRecursive,
        ),
        Target::MarkdownDir { path } => (path.clone(), RecursiveMode::Recursive),
        Target::Diff { repo, .. } => (repo.clone(), RecursiveMode::Recursive),
    };
    watcher.watch(&watch_path, mode)?;

    let target = target.clone();
    tokio::spawn(async move {
        while let Some(mut paths) = rx.recv().await {
            tokio::time::sleep(DEBOUNCE).await;
            while let Ok(more) = rx.try_recv() {
                paths.extend(more);
            }
            paths.sort();
            paths.dedup();
            let target = target.clone();
            let relevant = tokio::task::spawn_blocking(move || is_relevant(&target, paths))
                .await
                .unwrap_or(true);
            if relevant {
                let _ = changed.send(());
            }
        }
    });
    Ok(watcher)
}

fn is_relevant(target: &Target, paths: Vec<PathBuf>) -> bool {
    match target {
        Target::MarkdownFile { path } => paths.iter().any(|p| p == path),
        Target::MarkdownDir { .. } => paths.iter().any(|p| is_document_or_image(p)),
        Target::Diff { repo, .. } => {
            let (worktree, refs_changed) = split_git_paths(&repo.join(".git"), paths);
            if refs_changed {
                return true;
            }
            if worktree.is_empty() {
                return false;
            }
            match Git::new(repo).ignored(&worktree) {
                Ok(ignored) => worktree.len() > ignored.len(),
                Err(_) => true,
            }
        }
    }
}

/// Separates working tree paths from paths inside `.git`, keeping only the
/// `.git` changes that move HEAD or a ref. Objects, logs and lock files
/// churn on every git command and say nothing new on their own.
fn split_git_paths(git_dir: &Path, paths: Vec<PathBuf>) -> (Vec<PathBuf>, bool) {
    let mut worktree = Vec::new();
    let mut refs_changed = false;
    for p in paths {
        let Ok(rel) = p.strip_prefix(git_dir) else {
            worktree.push(p);
            continue;
        };
        if rel.extension().is_some_and(|e| e == "lock") {
            continue;
        }
        if rel == Path::new("HEAD") || rel == Path::new("packed-refs") || rel.starts_with("refs") {
            refs_changed = true;
        }
    }
    (worktree, refs_changed)
}

fn is_document_or_image(p: &Path) -> bool {
    let Some(ext) = p.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    ext == "md" || ext == "markdown" || crate::server::image_content_type(&ext).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_internals_matter_only_when_refs_move() {
        let git = Path::new("/r/.git");
        let (wt, refs) = split_git_paths(
            git,
            vec![
                "/r/.git/objects/ab/cd".into(),
                "/r/.git/index.lock".into(),
                "/r/src/a.rs".into(),
            ],
        );
        assert_eq!(wt, vec![PathBuf::from("/r/src/a.rs")]);
        assert!(!refs);

        let (_, refs) = split_git_paths(git, vec!["/r/.git/refs/heads/main".into()]);
        assert!(refs);
        let (_, refs) = split_git_paths(git, vec!["/r/.git/HEAD".into()]);
        assert!(refs);
        let (_, refs) = split_git_paths(git, vec!["/r/.git/HEAD.lock".into()]);
        assert!(!refs);
    }

    #[test]
    fn markdown_file_view_reacts_only_to_its_file() {
        let target = Target::MarkdownFile {
            path: "/d/a.md".into(),
        };
        assert!(is_relevant(&target, vec!["/d/a.md".into()]));
        assert!(!is_relevant(&target, vec!["/d/b.md".into()]));
    }

    #[test]
    fn markdown_dir_view_reacts_to_documents_and_images() {
        let target = Target::MarkdownDir { path: "/d".into() };
        assert!(is_relevant(&target, vec!["/d/x/IMG.PNG".into()]));
        assert!(!is_relevant(&target, vec!["/d/x/a.rs".into()]));
    }
}
