#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Deleted,
    Modified,
    Renamed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<Line>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub status: FileStatus,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    pub fn path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or_default()
    }

    pub fn additions(&self) -> usize {
        self.count(LineKind::Added)
    }

    pub fn deletions(&self) -> usize {
        self.count(LineKind::Deleted)
    }

    fn count(&self, kind: LineKind) -> usize {
        self.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == kind)
            .count()
    }
}

/// Parses the output of `git diff --src-prefix=a/ --dst-prefix=b/`.
pub fn parse(input: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut old_no = 0;
    let mut new_no = 0;

    for raw in input.split_inclusive('\n') {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let (old, new) = split_diff_git_paths(rest);
            files.push(FileDiff {
                old_path: old,
                new_path: new,
                status: FileStatus::Modified,
                binary: false,
                hunks: Vec::new(),
            });
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };

        if let Some(hunk) = file.hunks.last_mut() {
            let kind = match line.as_bytes().first() {
                Some(b' ') => Some(LineKind::Context),
                Some(b'+') => Some(LineKind::Added),
                Some(b'-') => Some(LineKind::Deleted),
                _ => None,
            };
            if let Some(kind) = kind {
                let (o, n) = match kind {
                    LineKind::Context => (Some(old_no), Some(new_no)),
                    LineKind::Added => (None, Some(new_no)),
                    LineKind::Deleted => (Some(old_no), None),
                };
                if o.is_some() {
                    old_no += 1;
                }
                if n.is_some() {
                    new_no += 1;
                }
                hunk.lines.push(Line {
                    kind,
                    old_no: o,
                    new_no: n,
                    text: line[1..].to_string(),
                });
                continue;
            }
            if line.starts_with('\\') {
                continue;
            }
        }

        if let Some(rest) = line.strip_prefix("@@ ") {
            let (o, n) = parse_hunk_start(rest);
            old_no = o;
            new_no = n;
            file.hunks.push(Hunk {
                header: line.to_string(),
                lines: Vec::new(),
            });
        } else if line.starts_with("new file mode") {
            file.status = FileStatus::Added;
            file.old_path = None;
        } else if line.starts_with("deleted file mode") {
            file.status = FileStatus::Deleted;
            file.new_path = None;
        } else if let Some(p) = line.strip_prefix("rename from ") {
            file.status = FileStatus::Renamed;
            file.old_path = Some(unquote(p));
        } else if let Some(p) = line.strip_prefix("rename to ") {
            file.status = FileStatus::Renamed;
            file.new_path = Some(unquote(p));
        } else if let Some(p) = line.strip_prefix("--- ") {
            file.old_path = header_path(p, "a/");
        } else if let Some(p) = line.strip_prefix("+++ ") {
            file.new_path = header_path(p, "b/");
        } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
            file.binary = true;
        }
    }
    files
}

fn header_path(p: &str, prefix: &str) -> Option<String> {
    let p = p.trim_end_matches('\t');
    if p == "/dev/null" {
        return None;
    }
    let p = unquote(p);
    Some(p.strip_prefix(prefix).map(str::to_string).unwrap_or(p))
}

/// `a/x b/y` is ambiguous when paths contain spaces; the `---`/`+++` or
/// `rename` lines override this guess whenever git prints them.
fn split_diff_git_paths(rest: &str) -> (Option<String>, Option<String>) {
    if rest.starts_with('"')
        && let Some(end) = closing_quote(rest)
    {
        let old = unquote(&rest[..=end]);
        let new = unquote(rest[end + 1..].trim_start());
        return (strip(&old, "a/"), strip(&new, "b/"));
    }
    if let Some(idx) = rest.find(" b/") {
        let new = &rest[idx + 1..];
        let new = unquote(new);
        return (strip(&rest[..idx], "a/"), strip(&new, "b/"));
    }
    (None, None)
}

fn strip(p: &str, prefix: &str) -> Option<String> {
    Some(p.strip_prefix(prefix).unwrap_or(p).to_string())
}

fn closing_quote(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// Undoes git's C-style quoting of paths with special characters.
fn unquote(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return s.to_string();
    };
    let mut out = Vec::new();
    let bytes = inner.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let c = bytes[i + 1];
        i += 2;
        match c {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'"' => out.push(b'"'),
            b'\\' => out.push(b'\\'),
            b'0'..=b'7' => {
                let mut v = (c - b'0') as u32;
                for _ in 0..2 {
                    if let Some(d @ b'0'..=b'7') = bytes.get(i) {
                        v = v * 8 + (d - b'0') as u32;
                        i += 1;
                    }
                }
                out.push(v as u8);
            }
            other => out.push(other),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_hunk_start(rest: &str) -> (u32, u32) {
    let mut parts = rest.split_whitespace();
    let start = |s: Option<&str>, sign: char| {
        s.and_then(|s| s.strip_prefix(sign))
            .and_then(|s| s.split(',').next())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    };
    let old = start(parts.next(), '-');
    let new = start(parts.next(), '+');
    (old, new)
}

/// Pairs lines for side-by-side display. A run of deletions followed by a
/// run of additions is laid out next to each other, as GitHub does.
pub fn split_rows(lines: &[Line]) -> Vec<(Option<&Line>, Option<&Line>)> {
    let mut rows = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind == LineKind::Context {
            rows.push((Some(&lines[i]), Some(&lines[i])));
            i += 1;
            continue;
        }
        let del_start = i;
        while i < lines.len() && lines[i].kind == LineKind::Deleted {
            i += 1;
        }
        let add_start = i;
        while i < lines.len() && lines[i].kind == LineKind::Added {
            i += 1;
        }
        let dels = &lines[del_start..add_start];
        let adds = &lines[add_start..i];
        for k in 0..dels.len().max(adds.len()) {
            rows.push((dels.get(k), adds.get(k)));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modified_file_with_line_numbers() {
        let input = "\
diff --git a/src/main.rs b/src/main.rs
index 1111111..2222222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,3 @@ fn main() {
 a
-b
+c
 d
";
        let files = parse(input);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.status, FileStatus::Modified);
        assert_eq!(f.path(), "src/main.rs");
        assert_eq!(f.additions(), 1);
        assert_eq!(f.deletions(), 1);
        let h = &f.hunks[0];
        assert_eq!(h.header, "@@ -1,3 +1,3 @@ fn main() {");
        let nos: Vec<_> = h.lines.iter().map(|l| (l.old_no, l.new_no)).collect();
        assert_eq!(
            nos,
            vec![
                (Some(1), Some(1)),
                (Some(2), None),
                (None, Some(2)),
                (Some(3), Some(3))
            ]
        );
        assert_eq!(h.lines[2].text, "c");
    }

    #[test]
    fn parses_added_deleted_and_multiple_files() {
        let input = "\
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..1111111
--- /dev/null
+++ b/new.txt
@@ -0,0 +1 @@
+hello
diff --git a/old.txt b/old.txt
deleted file mode 100644
index 1111111..0000000
--- a/old.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
";
        let files = parse(input);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].status, FileStatus::Added);
        assert_eq!(files[0].old_path, None);
        assert_eq!(files[0].path(), "new.txt");
        assert_eq!(files[1].status, FileStatus::Deleted);
        assert_eq!(files[1].new_path, None);
        assert_eq!(files[1].path(), "old.txt");
    }

    #[test]
    fn parses_rename_without_content_change() {
        let input = "\
diff --git a/a b.txt b/c d.txt
similarity index 100%
rename from a b.txt
rename to c d.txt
";
        let f = &parse(input)[0];
        assert_eq!(f.status, FileStatus::Renamed);
        assert_eq!(f.old_path.as_deref(), Some("a b.txt"));
        assert_eq!(f.new_path.as_deref(), Some("c d.txt"));
        assert!(f.hunks.is_empty());
    }

    #[test]
    fn parses_binary_file() {
        let input = "\
diff --git a/img.png b/img.png
index 1111111..2222222 100644
Binary files a/img.png and b/img.png differ
";
        let f = &parse(input)[0];
        assert!(f.binary);
        assert_eq!(f.path(), "img.png");
    }

    #[test]
    fn unquotes_paths_with_special_characters() {
        let input = "\
diff --git \"a/\\346\\227\\245.md\" \"b/\\346\\227\\245.md\"
--- \"a/\\346\\227\\245.md\"
+++ \"b/\\346\\227\\245.md\"
@@ -1 +1 @@
-x
+y
";
        let f = &parse(input)[0];
        assert_eq!(f.old_path.as_deref(), Some("日.md"));
        assert_eq!(f.new_path.as_deref(), Some("日.md"));
    }

    #[test]
    fn ignores_no_newline_marker_and_keeps_lines_starting_with_diff_markers() {
        let input = "\
diff --git a/a b/a
--- a/a
+++ b/a
@@ -1 +1,2 @@
--- not a header
\\ No newline at end of file
+++ also content
+@@ not a hunk
";
        let f = &parse(input)[0];
        let texts: Vec<_> = f.hunks[0].lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["-- not a header", "++ also content", "@@ not a hunk"]
        );
        assert_eq!(f.hunks.len(), 1);
    }

    #[test]
    fn split_rows_pair_deletions_with_following_additions() {
        let input = "\
diff --git a/a b/a
--- a/a
+++ b/a
@@ -1,4 +1,4 @@
 ctx
-d1
-d2
+a1
 ctx2
+a2
";
        let lines = &parse(input)[0].hunks[0].lines;
        let rows: Vec<_> = split_rows(lines)
            .into_iter()
            .map(|(l, r)| (l.map(|l| l.text.as_str()), r.map(|r| r.text.as_str())))
            .collect();
        assert_eq!(
            rows,
            vec![
                (Some("ctx"), Some("ctx")),
                (Some("d1"), Some("a1")),
                (Some("d2"), None),
                (Some("ctx2"), Some("ctx2")),
                (None, Some("a2")),
            ]
        );
    }
}
