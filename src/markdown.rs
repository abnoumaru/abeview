use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::path::{Component, Path};
use std::sync::Arc;

use comrak::adapters::{CodefenceRendererAdapter, SyntaxHighlighterAdapter};
use comrak::nodes::Sourcepos;
use comrak::options::Plugins;
use comrak::{Options, markdown_to_html_with_plugins};

use crate::highlight::{escape_into, highlighter};

/// Renders GitHub Flavored Markdown. Raw HTML in the source is omitted
/// rather than passed through, so a document cannot inject scripts.
///
/// `image_url` maps a relative image path (already resolved against the
/// document's directory) to the URL the browser should load it from.
pub fn render(
    source: &str,
    doc_dir: &Path,
    image_url: impl Fn(&str) -> String + Send + Sync + std::panic::RefUnwindSafe + 'static,
) -> String {
    let doc_dir = doc_dir.to_path_buf();
    let mut options = Options::default();
    let ext = &mut options.extension;
    ext.strikethrough = true;
    ext.table = true;
    ext.autolink = true;
    ext.tasklist = true;
    ext.footnotes = true;
    ext.alerts = true;
    ext.header_id_prefix = Some(String::new());
    ext.front_matter_delimiter = Some("---".into());
    ext.image_url_rewriter = Some(Arc::new(move |url: &str| {
        match resolve_relative(&doc_dir, url) {
            Some(path) => image_url(&path),
            None => url.to_string(),
        }
    }));
    options.render.tasklist_classes = true;

    let mut plugins = Plugins::default();
    plugins.render.codefence_syntax_highlighter = Some(&CodeHighlighter);
    plugins
        .render
        .codefence_renderers
        .insert("mermaid".into(), &MermaidRenderer);
    markdown_to_html_with_plugins(source, &options, &plugins)
}

/// Resolves `url` against `doc_dir` when it is a plain relative path.
/// Returns `None` for absolute URLs and for paths that climb above the root.
pub fn resolve_relative(doc_dir: &Path, url: &str) -> Option<String> {
    let is_external = url.is_empty()
        || url.starts_with('/')
        || url.starts_with('#')
        || url.contains("://")
        || url
            .split_once(':')
            .is_some_and(|(scheme, _)| !scheme.contains('/'));
    if is_external {
        return None;
    }
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let mut parts: Vec<String> = Vec::new();
    for c in doc_dir.join(path).components() {
        match c {
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts.join("/"))
}

struct CodeHighlighter;

impl SyntaxHighlighterAdapter for CodeHighlighter {
    fn write_highlighted(
        &self,
        output: &mut dyn fmt::Write,
        lang: Option<&str>,
        code: &str,
    ) -> fmt::Result {
        let h = highlighter();
        let syntax = match lang {
            Some(lang) => h.syntax_for_lang(lang),
            None => h.syntax_for_lang("txt"),
        };
        let mut lines = h.lines(syntax);
        for line in code.lines() {
            output.write_str(&lines.line(line))?;
            output.write_char('\n')?;
        }
        Ok(())
    }

    fn write_pre_tag(
        &self,
        output: &mut dyn fmt::Write,
        _attributes: HashMap<&'static str, Cow<'_, str>>,
    ) -> fmt::Result {
        output.write_str("<pre class=\"hl\">")
    }

    fn write_code_tag(
        &self,
        output: &mut dyn fmt::Write,
        _attributes: HashMap<&'static str, Cow<'_, str>>,
    ) -> fmt::Result {
        output.write_str("<code>")
    }
}

struct MermaidRenderer;

impl CodefenceRendererAdapter for MermaidRenderer {
    fn write(
        &self,
        output: &mut dyn fmt::Write,
        _lang: &str,
        _meta: &str,
        code: &str,
        _sourcepos: Option<Sourcepos>,
    ) -> fmt::Result {
        let mut escaped = String::new();
        escape_into(&mut escaped, code);
        writeln!(output, "<pre class=\"mermaid\">{escaped}</pre>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(source: &str) -> String {
        render(source, Path::new("docs"), |p| format!("/raw/{p}"))
    }

    #[test]
    fn renders_gfm_tables_and_task_lists() {
        let html = md("| a |\n|---|\n| b |\n\n- [x] done\n");
        assert!(html.contains("<table>"));
        assert!(html.contains("task-list-item"));
    }

    #[test]
    fn omits_raw_html_and_dangerous_links() {
        let html = md("<script>alert(1)</script>\n\n[x](javascript:alert(1))\n");
        assert!(!html.contains("<script>"));
        assert!(!html.contains("javascript:"));
    }

    #[test]
    fn highlights_fenced_code() {
        let html = md("```rust\nfn main() {}\n```\n");
        assert!(html.contains("<pre class=\"hl\"><code>"));
        assert!(html.contains("--l:#"));
    }

    #[test]
    fn renders_mermaid_blocks_for_the_browser() {
        let html = md("```mermaid\ngraph TD; A-->B\n```\n");
        assert!(
            html.contains("<pre class=\"mermaid\">graph TD; A--&gt;B\n</pre>"),
            "{html}"
        );
    }

    #[test]
    fn rewrites_relative_images_only() {
        let html = md("![a](img/a.png) ![b](../b.png) ![c](https://example.com/c.png)\n");
        assert!(html.contains("src=\"/raw/docs/img/a.png\""), "{html}");
        assert!(html.contains("src=\"/raw/b.png\""));
        assert!(html.contains("src=\"https://example.com/c.png\""));
    }

    #[test]
    fn resolve_relative_refuses_to_climb_above_root() {
        assert_eq!(resolve_relative(Path::new(""), "../x.png"), None);
        assert_eq!(
            resolve_relative(Path::new("a"), "./b/../c.png").as_deref(),
            Some("a/c.png")
        );
        assert_eq!(
            resolve_relative(Path::new(""), "data:image/png;base64,xx"),
            None
        );
    }

    #[test]
    fn hides_front_matter() {
        let html = md("---\ntitle: x\n---\n# Hello\n");
        assert!(!html.contains("title: x"));
        assert!(html.contains("Hello"));
    }
}
