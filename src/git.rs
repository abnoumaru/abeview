use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::target::DiffSpec;

const DIFF_OPTIONS: &[&str] = &[
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--src-prefix=a/",
    "--dst-prefix=b/",
    "-M",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    /// Relative, such as "3 hours ago".
    pub date: String,
    pub subject: String,
    pub body: String,
}

pub struct Git {
    repo: PathBuf,
}

impl Git {
    pub fn new(repo: impl Into<PathBuf>) -> Self {
        Self { repo: repo.into() }
    }

    pub fn toplevel(dir: &Path) -> Result<PathBuf> {
        let out = Git::new(dir).run(&["rev-parse", "--show-toplevel"])?;
        Ok(PathBuf::from(out.trim_end()))
    }

    /// Rejects anything git could read as an option, then resolves it to a commit.
    pub fn verify_commit(&self, rev: &str) -> Result<()> {
        if rev.starts_with('-') {
            bail!("invalid revision: {rev}");
        }
        let resolved = self.run(&[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &format!("{rev}^{{commit}}"),
        ]);
        if resolved.is_err() {
            bail!("unknown revision: {rev}");
        }
        Ok(())
    }

    pub fn default_branch(&self) -> Result<String> {
        if let Ok(out) = self.run(&[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ]) {
            return Ok(out.trim_end().to_string());
        }
        for candidate in ["main", "master"] {
            if self.verify_commit(candidate).is_ok() {
                return Ok(candidate.to_string());
            }
        }
        bail!("could not determine the default branch; pass --base")
    }

    pub fn diff(&self, spec: &DiffSpec) -> Result<String> {
        for rev in spec.revisions() {
            self.verify_commit(rev)?;
        }
        match spec {
            DiffSpec::WorkingTree => self.working_tree_diff(),
            DiffSpec::MergeBase { from, to } => {
                self.run_diff(&["diff"], &[&format!("{from}...{to}")])
            }
            DiffSpec::Range { from, to } => self.run_diff(&["diff"], &[from, to]),
            DiffSpec::Commit { rev } => {
                self.run_diff(&["show", "--format=", "--diff-merges=first-parent"], &[rev])
            }
        }
    }

    /// Commits the diff is made of, oldest first.
    pub fn commits(&self, spec: &DiffSpec) -> Result<Vec<Commit>> {
        const FORMAT: &str = "--format=%H%x00%h%x00%an%x00%ar%x00%s%x00%b%x1e";
        const LIMIT: &str = "--max-count=250";
        for rev in spec.revisions() {
            self.verify_commit(rev)?;
        }
        let range;
        let args: Vec<&str> = match spec {
            DiffSpec::WorkingTree => return Ok(Vec::new()),
            DiffSpec::MergeBase { from, to } | DiffSpec::Range { from, to } => {
                range = format!("{from}..{to}");
                vec![
                    "log",
                    "--no-show-signature",
                    "--reverse",
                    LIMIT,
                    FORMAT,
                    "--end-of-options",
                    &range,
                    "--",
                ]
            }
            DiffSpec::Commit { rev } => {
                vec![
                    "log",
                    "--no-show-signature",
                    "-1",
                    FORMAT,
                    "--end-of-options",
                    rev,
                    "--",
                ]
            }
        };
        let out = self.run(&args)?;
        Ok(out
            .split('\x1e')
            .filter_map(|record| {
                let mut f = record.trim_start_matches('\n').split('\0');
                Some(Commit {
                    hash: f.next().filter(|h| !h.is_empty())?.to_string(),
                    short_hash: f.next()?.to_string(),
                    author: f.next()?.to_string(),
                    date: f.next()?.to_string(),
                    subject: f.next()?.to_string(),
                    body: f.next()?.trim_end().to_string(),
                })
            })
            .collect())
    }

    /// Content of `path` on the "after" side of the diff.
    pub fn new_side_content(&self, spec: &DiffSpec, path: &str) -> Result<Vec<u8>> {
        let rev = match spec {
            DiffSpec::WorkingTree => {
                return std::fs::read(self.repo.join(path))
                    .with_context(|| format!("failed to read {path}"));
            }
            DiffSpec::MergeBase { to, .. } | DiffSpec::Range { to, .. } => to,
            DiffSpec::Commit { rev } => rev,
        };
        self.verify_commit(rev)?;
        let out = self
            .command(&["show", "--end-of-options", &format!("{rev}:{path}")])
            .output()?;
        if !out.status.success() {
            bail!("git show failed: {}", String::from_utf8_lossy(&out.stderr));
        }
        Ok(out.stdout)
    }

    /// Returns which of `paths` are ignored by .gitignore and friends.
    pub fn ignored(&self, paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
        use std::io::Write;
        use std::os::unix::ffi::OsStrExt;

        let mut child = self
            .command(&["check-ignore", "-z", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        for p in paths {
            stdin.write_all(p.as_os_str().as_bytes())?;
            stdin.write_all(b"\0")?;
        }
        drop(stdin);
        let out = child.wait_with_output()?;
        Ok(out
            .stdout
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| PathBuf::from(std::ffi::OsStr::from_bytes(s)))
            .collect())
    }

    fn working_tree_diff(&self) -> Result<String> {
        let base = if self.verify_commit("HEAD").is_ok() {
            "HEAD".to_string()
        } else {
            self.run(&["hash-object", "-t", "tree", "/dev/null"])?
                .trim_end()
                .to_string()
        };
        let mut out = self.run_diff(&["diff"], &[&base])?;

        let untracked = self.run(&["ls-files", "--others", "--exclude-standard", "-z"])?;
        // Nested repositories are listed as `dir/` and cannot be diffed as files.
        for path in untracked
            .split('\0')
            .filter(|p| !p.is_empty() && !p.ends_with('/'))
        {
            let mut args = vec!["diff", "--no-index"];
            args.extend_from_slice(DIFF_OPTIONS);
            args.extend_from_slice(&["--", "/dev/null", path]);
            // `--no-index` exits with 1 when the files differ, which they always do here.
            let output = self.command(&args).output()?;
            if output.status.code() != Some(1) {
                bail!("git diff --no-index failed for {path}");
            }
            out.push_str(&String::from_utf8_lossy(&output.stdout));
        }
        Ok(out)
    }

    fn run_diff(&self, command: &[&str], revs: &[&str]) -> Result<String> {
        let mut args = command.to_vec();
        args.extend_from_slice(DIFF_OPTIONS);
        args.push("--end-of-options");
        args.extend_from_slice(revs);
        args.push("--");
        self.run(&args)
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let out = self.command(args).output().context("failed to run git")?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr).trim_end()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        // Keeps `git diff` from refreshing the index, which would wake our own file watcher.
        cmd.env("GIT_OPTIONAL_LOCKS", "0")
            .arg("-C")
            .arg(&self.repo)
            .args(["-c", "core.quotepath=off"])
            .args(args)
            .stdin(Stdio::null());
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{self, FileStatus};

    fn sh(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        sh(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        sh(dir.path(), &["add", "."]);
        sh(dir.path(), &["commit", "-q", "-m", "init"]);
        dir
    }

    #[test]
    fn working_tree_includes_modified_and_untracked_files() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();
        std::fs::write(dir.path().join("new.md"), "# hi\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored\n").unwrap();
        std::fs::write(dir.path().join("ignored"), "x\n").unwrap();

        let files = diff::parse(&Git::new(dir.path()).diff(&DiffSpec::WorkingTree).unwrap());
        let summary: Vec<_> = files.iter().map(|f| (f.path(), f.status)).collect();
        assert_eq!(
            summary,
            vec![
                ("a.txt", FileStatus::Modified),
                (".gitignore", FileStatus::Added),
                ("new.md", FileStatus::Added),
            ]
        );
    }

    #[test]
    fn working_tree_works_before_first_commit() {
        let dir = tempfile::tempdir().unwrap();
        sh(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
        sh(dir.path(), &["add", "."]);

        let files = diff::parse(&Git::new(dir.path()).diff(&DiffSpec::WorkingTree).unwrap());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Added);
    }

    #[test]
    fn merge_base_diff_shows_only_branch_changes() {
        let dir = repo();
        sh(dir.path(), &["switch", "-q", "-c", "feat"]);
        std::fs::write(dir.path().join("feat.txt"), "f\n").unwrap();
        sh(dir.path(), &["add", "."]);
        sh(dir.path(), &["commit", "-q", "-m", "feat"]);
        sh(dir.path(), &["switch", "-q", "main"]);
        std::fs::write(dir.path().join("main.txt"), "m\n").unwrap();
        sh(dir.path(), &["add", "."]);
        sh(dir.path(), &["commit", "-q", "-m", "main"]);

        let git = Git::new(dir.path());
        let spec = DiffSpec::MergeBase {
            from: "main".into(),
            to: "feat".into(),
        };
        let files = diff::parse(&git.diff(&spec).unwrap());
        let paths: Vec<_> = files.iter().map(|f| f.path()).collect();
        assert_eq!(paths, vec!["feat.txt"]);
        assert_eq!(git.new_side_content(&spec, "feat.txt").unwrap(), b"f\n");
    }

    #[test]
    fn commit_diff_works_for_root_commit() {
        let dir = repo();
        let files = diff::parse(
            &Git::new(dir.path())
                .diff(&DiffSpec::Commit { rev: "HEAD".into() })
                .unwrap(),
        );
        assert_eq!(files[0].path(), "a.txt");
        assert_eq!(files[0].status, FileStatus::Added);
    }

    #[test]
    fn rejects_revisions_that_look_like_options() {
        let dir = repo();
        let spec = DiffSpec::Commit {
            rev: "--output=/tmp/x".into(),
        };
        assert!(Git::new(dir.path()).diff(&spec).is_err());
    }

    fn commit(dir: &Path, file: &str, message: &str) {
        std::fs::write(dir.join(file), message).unwrap();
        sh(dir, &["add", "."]);
        sh(dir, &["commit", "-q", "-m", message]);
    }

    #[test]
    fn lists_branch_commits_oldest_first() {
        let dir = repo();
        sh(dir.path(), &["switch", "-q", "-c", "feat"]);
        commit(dir.path(), "b.txt", "first\n\nwhy it was needed");
        commit(dir.path(), "c.txt", "second");
        let git = Git::new(dir.path());
        let spec = DiffSpec::MergeBase {
            from: "main".into(),
            to: "feat".into(),
        };
        let commits = git.commits(&spec).unwrap();
        let subjects: Vec<_> = commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, vec!["first", "second"]);
        assert_eq!(commits[0].body, "why it was needed");
        assert_eq!(commits[0].author, "t");
        assert_eq!(commits[0].short_hash.len(), 7);
    }

    #[test]
    fn single_commit_spec_lists_that_commit() {
        let dir = repo();
        let commits = Git::new(dir.path())
            .commits(&DiffSpec::Commit { rev: "HEAD".into() })
            .unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "init");
    }

    #[test]
    fn working_tree_has_no_commits() {
        let dir = repo();
        assert!(
            Git::new(dir.path())
                .commits(&DiffSpec::WorkingTree)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn default_branch_falls_back_to_main() {
        let dir = repo();
        assert_eq!(Git::new(dir.path()).default_branch().unwrap(), "main");
    }

    #[test]
    fn reports_ignored_paths() {
        let dir = repo();
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        let git = Git::new(dir.path());
        let ignored = git
            .ignored(&[dir.path().join("target/x"), dir.path().join("a.txt")])
            .unwrap();
        assert_eq!(ignored, vec![dir.path().join("target/x")]);
    }
}
