use maud::{DOCTYPE, Markup, PreEscaped, html};

use crate::diff::{self, FileDiff, FileStatus, Hunk, LineKind};
use crate::highlight::{escape_into, highlighter};
use crate::tree::{self, TreeNode};

/// Files larger than this are shown without syntax highlighting.
const HIGHLIGHT_LIMIT: usize = 3_000;
/// Files larger than this are not rendered at all, like GitHub's "Load diff".
const RENDER_LIMIT: usize = 20_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Diff,
    Markdown,
}

pub fn page(view_id: &str, kind: Kind, title: &str, content: Markup) -> Markup {
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
            body data-view=(view_id) data-kind=(if kind == Kind::Diff { "diff" } else { "markdown" }) {
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

pub fn diff_content(files: &[FileDiff]) -> Markup {
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
            div.summary {
                "Showing " strong { (files.len()) " changed files" }
                " with " span.add { (additions) " additions" }
                " and " span.del { (deletions) " deletions" }
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
                    li.tree-file {
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
    let total: usize = file.hunks.iter().map(|h| h.lines.len()).sum();
    let is_markdown = is_markdown(file.path()) && file.status != FileStatus::Deleted;
    html! {
        section.file id=(format!("f-{index}")) data-path=(file.path()) {
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
            }
            div.file-body {
                @if file.binary {
                    div.note { "Binary file not shown." }
                } @else if file.hunks.is_empty() {
                    div.note {
                        @if file.status == FileStatus::Renamed { "File renamed without changes." }
                        @else { "No content changes." }
                    }
                } @else if total > RENDER_LIMIT {
                    div.note { "Large diff (" (total) " lines) not rendered." }
                } @else {
                    (diff_tables(file, total <= HIGHLIGHT_LIMIT))
                }
                @if is_markdown {
                    div.md-preview.markdown-body hidden {}
                }
            }
        }
    }
}

fn diff_tables(file: &FileDiff, highlight: bool) -> Markup {
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
        let html = diff_content(&diff::parse(INPUT)).into_string();
        assert!(html.contains("table class=\"diff unified\""));
        assert!(html.contains("table class=\"diff split\""));
        assert!(html.contains("&lt;b&gt;"));
        assert!(!html.contains("<b>"));
    }

    #[test]
    fn offers_preview_only_for_markdown_files() {
        let html = diff_content(&diff::parse(INPUT)).into_string();
        assert_eq!(html.matches("data-display=\"preview\"").count(), 1);
    }

    #[test]
    fn orders_file_blocks_like_the_tree() {
        let html = diff_content(&diff::parse(INPUT)).into_string();
        let src = html.find("data-path=\"src/x.zzz\"").unwrap();
        let readme = html.find("data-path=\"README.md\"").unwrap();
        assert!(src < readme);
    }

    #[test]
    fn percent_encodes_query_values() {
        assert_eq!(percent_encode("docs/a b&c.md"), "docs/a%20b%26c.md");
    }
}
