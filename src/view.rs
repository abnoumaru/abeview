use maud::{DOCTYPE, Markup, PreEscaped, html};

use crate::diff::{self, FileDiff, FileStatus, Hunk, LineKind};
use crate::git::Commit;
use crate::highlight::{escape_into, highlighter};
use crate::tree::{self, TreeNode};

/// Files larger than this are shown without syntax highlighting.
const HIGHLIGHT_LIMIT: usize = 3_000;
/// Files with more changed lines than this wait for "Load diff", like GitHub.
const COLLAPSE_LIMIT: usize = 400;

const GENERATED_FILE_NAMES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "Gemfile.lock",
    "composer.lock",
    "poetry.lock",
    "uv.lock",
    "go.sum",
    "flake.lock",
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Diff,
    Markdown,
}

/// `state_key` identifies the target across server restarts, for state the
/// browser keeps such as which files were marked as viewed.
pub fn page(view_id: &str, state_key: &str, kind: Kind, title: &str, content: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " · abeview" }
                link rel="stylesheet" href="/assets/github-markdown.css";
                link rel="stylesheet" href="/assets/app.css";
            }
            body data-view=(view_id) data-state-key=(state_key) data-kind=(if kind == Kind::Diff { "diff" } else { "markdown" }) {
                header.topbar {
                    span.brand { "abeview" }
                    h1.title { (title) }
                    @if kind == Kind::Diff {
                        div.toggle role="group" aria-label="Diff layout" {
                            button type="button" data-layout="unified" { "Unified" }
                            button type="button" data-layout="split" { "Split" }
                        }
                    }
                    span #live.live title="Disconnected" { "●" }
                }
                div #app { (content) }
                script src="/assets/mermaid.min.js" {}
                script src="/assets/app.js" {}
            }
        }
    }
}

pub fn error_content(message: &str) -> Markup {
    html! {
        main.content {
            div.error { pre { (message) } }
        }
    }
}

/// What is shown above the diff, like a pull request's description.
#[derive(Default)]
pub struct Description {
    /// Explanation passed in by whoever opened the view, already rendered.
    pub note_html: Option<String>,
    pub history: History,
}

#[derive(Default)]
pub enum History {
    #[default]
    None,
    Commits(Vec<Commit>),
    /// A single commit, shown with its whole message.
    Message(Commit),
}

fn description_block(description: &Description) -> Markup {
    let has_history = match &description.history {
        History::None => false,
        History::Commits(commits) => !commits.is_empty(),
        History::Message(_) => true,
    };
    if description.note_html.is_none() && !has_history {
        return html! {};
    }
    html! {
        section.description {
            @if let Some(note) = &description.note_html {
                div.note-body.markdown-body { (PreEscaped(note)) }
            }
            @match &description.history {
                History::None => {}
                History::Commits(commits) => @if !commits.is_empty() {
                    details.commits open {
                        summary { "Commits " span.count { (commits.len()) } }
                        ol.commit-list {
                            @for c in commits {
                                li title=(c.hash) {
                                    code.sha { (c.short_hash) }
                                    span.subject { (c.subject) }
                                    span.meta { (c.author) " · " (c.date) }
                                }
                            }
                        }
                    }
                }
                History::Message(c) => {
                    div.commit-message {
                        h2 { (c.subject) }
                        @if !c.body.is_empty() {
                            pre { (c.body) }
                        }
                        div.meta { code.sha { (c.short_hash) } " " (c.author) " · " (c.date) }
                    }
                }
            }
        }
    }
}

pub fn diff_content(files: &[FileDiff], description: &Description) -> Markup {
    let paths: Vec<&str> = files.iter().map(|f| f.path()).collect();
    let nodes = tree::build(&paths);
    let order = tree::file_order(&nodes);
    let additions: usize = files.iter().map(FileDiff::additions).sum();
    let deletions: usize = files.iter().map(FileDiff::deletions).sum();
    html! {
        nav.sidebar {
            div.sidebar-title { "Files changed " span.count { (files.len()) } }
            ul.tree { (diff_tree(&nodes, files)) }
        }
        main.content {
            (description_block(description))
            div.summary {
                "Showing " strong { (files.len()) " changed files" }
                " with " span.add { (additions) " additions" }
                " and " span.del { (deletions) " deletions" }
                span #viewed-progress.viewed-progress {}
            }
            @if files.is_empty() {
                div.empty { "No changes." }
            }
            @for i in order {
                (file_block(i, &files[i]))
            }
        }
    }
}

fn diff_tree(nodes: &[TreeNode], files: &[FileDiff]) -> Markup {
    html! {
        @for node in nodes {
            @match node {
                TreeNode::Dir { name, children } => {
                    li.tree-dir {
                        details open {
                            summary { (name) }
                            ul { (diff_tree(children, files)) }
                        }
                    }
                }
                TreeNode::File { name, index } => {
                    li.tree-file data-path=(files[*index].path()) {
                        a href=(format!("#f-{index}")) title=(files[*index].path()) {
                            span class=(format!("status {}", status_class(files[*index].status))) {
                                (status_letter(files[*index].status))
                            }
                            (name)
                        }
                    }
                }
            }
        }
    }
}

fn file_block(index: usize, file: &FileDiff) -> Markup {
    let changed = file.additions() + file.deletions();
    let is_markdown = is_markdown(file.path()) && file.status != FileStatus::Deleted;
    html! {
        section.file id=(format!("f-{index}")) data-path=(file.path()) data-hash=(file.fingerprint()) {
            header.file-header {
                button.collapse type="button" aria-label="Toggle file" { "▾" }
                span class=(format!("status {}", status_class(file.status))) {
                    (status_letter(file.status))
                }
                span.path {
                    @if file.status == FileStatus::Renamed {
                        (file.old_path.as_deref().unwrap_or_default()) " → "
                    }
                    (file.path())
                }
                span.stat {
                    span.add { "+" (file.additions()) }
                    " "
                    span.del { "−" (file.deletions()) }
                }
                @if is_markdown {
                    div.toggle.file-toggle role="group" aria-label="Markdown display" {
                        button type="button" data-display="diff" { "Diff" }
                        button type="button" data-display="preview" { "Preview" }
                    }
                }
                label.viewed-toggle {
                    input type="checkbox";
                    " Viewed"
                }
            }
            div.file-body {
                @if file.binary {
                    div.note { "Binary file not shown." }
                } @else if file.hunks.is_empty() {
                    div.note {
                        @if file.status == FileStatus::Renamed { "File renamed without changes." }
                        @else { "No content changes." }
                    }
                } @else if changed > COLLAPSE_LIMIT || is_generated(file.path()) {
                    div.load {
                        button.load-diff type="button" { "Load diff" }
                        span {
                            @if is_generated(file.path()) { "Generated file. " }
                            @else { "Large diff. " }
                            (changed) " changed lines are hidden."
                        }
                    }
                } @else {
                    (file_tables(file))
                }
                @if is_markdown {
                    div.md-preview.markdown-body hidden {}
                }
            }
        }
    }
}

pub fn file_tables(file: &FileDiff) -> Markup {
    let lines: usize = file.hunks.iter().map(|h| h.lines.len()).sum();
    let highlight = lines <= HIGHLIGHT_LIMIT;
    let highlighted: Vec<Vec<String>> = file
        .hunks
        .iter()
        .map(|h| highlight_hunk(file.path(), h, highlight))
        .collect();
    html! {
        div.scroll {
            table.diff.unified {
                @for (hunk, html) in file.hunks.iter().zip(&highlighted) {
                    tr.hunk { td.num {} td.num {} td.code { (hunk.header) } }
                    @for (line, code) in hunk.lines.iter().zip(html) {
                        tr class=(line_class(line.kind)) {
                            td.num { (num(line.old_no)) }
                            td.num { (num(line.new_no)) }
                            td.code { span.marker { (marker(line.kind)) } (PreEscaped(code)) }
                        }
                    }
                }
            }
        }
        div.scroll {
            table.diff.split {
                colgroup { col.num-col; col; col.num-col; col; }
                @for (hunk, html) in file.hunks.iter().zip(&highlighted) {
                    tr.hunk { td.num {} td.code colspan="3" { (hunk.header) } }
                    @for (left, right) in diff::split_rows(&hunk.lines) {
                        tr {
                            (split_cell(left, &hunk.lines, html, |l| l.old_no))
                            (split_cell(right, &hunk.lines, html, |l| l.new_no))
                        }
                    }
                }
            }
        }
    }
}

fn split_cell(
    line: Option<&diff::Line>,
    all: &[diff::Line],
    html: &[String],
    no: fn(&diff::Line) -> Option<u32>,
) -> Markup {
    let Some(line) = line else {
        return html! { td.num.empty {} td.code.empty {} };
    };
    let idx = all
        .iter()
        .position(|l| std::ptr::eq(l, line))
        .expect("line belongs to hunk");
    html! {
        td class=(format!("num {}", line_class(line.kind))) { (num(no(line))) }
        td class=(format!("code {}", line_class(line.kind))) {
            span.marker { (marker(line.kind)) } (PreEscaped(&html[idx]))
        }
    }
}

/// Highlights each side separately so that deleted and added code are both
/// parsed in their own context.
fn highlight_hunk(path: &str, hunk: &Hunk, enabled: bool) -> Vec<String> {
    if !enabled {
        return hunk
            .lines
            .iter()
            .map(|l| {
                let mut s = String::new();
                escape_into(&mut s, &l.text);
                s
            })
            .collect();
    }
    let h = highlighter();
    let syntax = h.syntax_for_path(path);
    let mut old = h.lines(syntax);
    let mut new = h.lines(syntax);
    hunk.lines
        .iter()
        .map(|l| match l.kind {
            LineKind::Context => {
                old.line(&l.text);
                new.line(&l.text)
            }
            LineKind::Deleted => old.line(&l.text),
            LineKind::Added => new.line(&l.text),
        })
        .collect()
}

pub fn markdown_content(sidebar: Option<(&[String], Option<&str>)>, body: &str) -> Markup {
    html! {
        @if let Some((files, current)) = sidebar {
            nav.sidebar {
                div.sidebar-title { "Documents " span.count { (files.len()) } }
                ul.tree { (markdown_tree(&tree::build(files), files, current)) }
            }
        }
        main.content {
            article.markdown-body { (PreEscaped(body)) }
        }
    }
}

fn markdown_tree(nodes: &[TreeNode], files: &[String], current: Option<&str>) -> Markup {
    html! {
        @for node in nodes {
            @match node {
                TreeNode::Dir { name, children } => {
                    li.tree-dir {
                        details open {
                            summary { (name) }
                            ul { (markdown_tree(children, files, current)) }
                        }
                    }
                }
                TreeNode::File { name, index } => {
                    @let path = &files[*index];
                    li.tree-file.current[current == Some(path.as_str())] {
                        a href=(format!("?file={}", percent_encode(path))) title=(path) { (name) }
                    }
                }
            }
        }
    }
}

/// Lock files and minified assets, whose diffs nobody reads line by line.
pub fn is_generated(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    GENERATED_FILE_NAMES.contains(&name) || name.ends_with(".min.js") || name.ends_with(".min.css")
}

pub fn is_markdown(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown")
}

pub fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn num(n: Option<u32>) -> String {
    n.map(|n| n.to_string()).unwrap_or_default()
}

fn marker(kind: LineKind) -> &'static str {
    match kind {
        LineKind::Context => " ",
        LineKind::Added => "+",
        LineKind::Deleted => "-",
    }
}

fn line_class(kind: LineKind) -> &'static str {
    match kind {
        LineKind::Context => "ctx",
        LineKind::Added => "add",
        LineKind::Deleted => "del",
    }
}

fn status_class(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Added => "st-added",
        FileStatus::Deleted => "st-deleted",
        FileStatus::Modified => "st-modified",
        FileStatus::Renamed => "st-renamed",
    }
}

fn status_letter(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Added => "A",
        FileStatus::Deleted => "D",
        FileStatus::Modified => "M",
        FileStatus::Renamed => "R",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INPUT: &str = "\
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-<old>
+<new>
diff --git a/src/x.zzz b/src/x.zzz
--- a/src/x.zzz
+++ b/src/x.zzz
@@ -1 +1 @@
-a
+<b>
";

    #[test]
    fn renders_both_layouts_with_escaped_code() {
        let html = diff_content(&diff::parse(INPUT), &Description::default()).into_string();
        assert!(html.contains("table class=\"diff unified\""));
        assert!(html.contains("table class=\"diff split\""));
        assert!(html.contains("&lt;b&gt;"));
        assert!(!html.contains("<b>"));
    }

    #[test]
    fn offers_viewed_checkbox_with_content_hash() {
        let files = diff::parse(INPUT);
        let html = diff_content(&files, &Description::default()).into_string();
        assert_eq!(html.matches("class=\"viewed-toggle\"").count(), 2);
        assert!(html.contains(&format!("data-hash=\"{}\"", files[0].fingerprint())));
    }

    #[test]
    fn offers_preview_only_for_markdown_files() {
        let html = diff_content(&diff::parse(INPUT), &Description::default()).into_string();
        assert_eq!(html.matches("data-display=\"preview\"").count(), 1);
    }

    #[test]
    fn orders_file_blocks_like_the_tree() {
        let html = diff_content(&diff::parse(INPUT), &Description::default()).into_string();
        let src = html.find("data-path=\"src/x.zzz\"").unwrap();
        let readme = html.find("data-path=\"README.md\"").unwrap();
        assert!(src < readme);
    }

    fn added_file(path: &str, lines: usize) -> String {
        let mut s = format!(
            "diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{lines} @@\n"
        );
        for i in 0..lines {
            s.push_str(&format!("+line {i}\n"));
        }
        s
    }

    #[test]
    fn defers_large_diffs_behind_load_button() {
        let html = diff_content(
            &diff::parse(&added_file("big.txt", 401)),
            &Description::default(),
        )
        .into_string();
        assert!(html.contains("class=\"load-diff\""));
        assert!(!html.contains("<table"));

        let html = diff_content(
            &diff::parse(&added_file("small.txt", 400)),
            &Description::default(),
        )
        .into_string();
        assert!(!html.contains("class=\"load-diff\""));
        assert!(html.contains("<table"));
    }

    #[test]
    fn defers_generated_files_regardless_of_size() {
        let html = diff_content(
            &diff::parse(&added_file("app/Cargo.lock", 1)),
            &Description::default(),
        )
        .into_string();
        assert!(html.contains("class=\"load-diff\""));
        assert!(!html.contains("<table"));
    }

    #[test]
    fn recognizes_generated_files() {
        for path in [
            "Cargo.lock",
            "web/package-lock.json",
            "yarn.lock",
            "go.sum",
            "a/b.min.js",
        ] {
            assert!(is_generated(path), "{path}");
        }
        assert!(!is_generated("src/lock.rs"));
    }

    #[test]
    fn renders_a_single_file_body_on_demand() {
        let files = diff::parse(&added_file("big.txt", 401));
        let html = file_tables(&files[0]).into_string();
        assert!(html.contains("table class=\"diff unified\""));
    }

    fn commit(subject: &str, body: &str) -> Commit {
        Commit {
            hash: "a".repeat(40),
            short_hash: "aaaaaaa".into(),
            author: "t".into(),
            date: "1 hour ago".into(),
            subject: subject.into(),
            body: body.into(),
        }
    }

    #[test]
    fn shows_note_and_commit_list_above_the_diff() {
        let description = Description {
            note_html: Some("<p>Why this change</p>".into()),
            history: History::Commits(vec![commit("<fix> parser", "")]),
        };
        let html = diff_content(&diff::parse(INPUT), &description).into_string();
        let note = html.find("Why this change").unwrap();
        let commits = html.find("&lt;fix&gt; parser").unwrap();
        let first_file = html.find("section class=\"file\"").unwrap();
        assert!(note < commits && commits < first_file);
    }

    #[test]
    fn shows_full_message_for_a_single_commit() {
        let description = Description {
            note_html: None,
            history: History::Message(commit("subject", "body line")),
        };
        let html = diff_content(&diff::parse(INPUT), &description).into_string();
        assert!(html.contains("body line"));
        assert!(!html.contains("class=\"note-body"));
    }

    #[test]
    fn omits_description_when_there_is_nothing_to_say() {
        let html = diff_content(&diff::parse(INPUT), &Description::default()).into_string();
        assert!(!html.contains("class=\"description\""));
    }

    #[test]
    fn percent_encodes_query_values() {
        assert_eq!(percent_encode("docs/a b&c.md"), "docs/a%20b%26c.md");
    }
}
