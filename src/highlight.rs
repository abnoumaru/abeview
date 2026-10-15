use std::path::Path;
use std::sync::LazyLock;

use syntect::highlighting::{Color, FontStyle, Theme};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use two_face::theme::EmbeddedThemeName;

pub struct Highlighter {
    syntaxes: SyntaxSet,
    light: Theme,
    dark: Theme,
}

static HIGHLIGHTER: LazyLock<Highlighter> = LazyLock::new(|| {
    let themes = two_face::theme::extra();
    Highlighter {
        syntaxes: two_face::syntax::extra_newlines(),
        light: themes.get(EmbeddedThemeName::Github).clone(),
        dark: themes.get(EmbeddedThemeName::OneHalfDark).clone(),
    }
});

pub fn highlighter() -> &'static Highlighter {
    &HIGHLIGHTER
}

impl Highlighter {
    pub fn syntax_for_path(&self, path: &str) -> &SyntaxReference {
        let path = Path::new(path);
        let by_ext = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(|e| self.syntaxes.find_syntax_by_extension(e));
        let by_name = || {
            path.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| self.syntaxes.find_syntax_by_extension(n))
        };
        by_ext
            .or_else(by_name)
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text())
    }

    pub fn syntax_for_lang(&self, lang: &str) -> &SyntaxReference {
        self.syntaxes
            .find_syntax_by_token(lang)
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text())
    }

    pub fn lines<'a>(&'a self, syntax: &SyntaxReference) -> LineHighlighter<'a> {
        LineHighlighter {
            owner: self,
            state: ParseState::new(syntax),
            stack: ScopeStack::new(),
            light: syntect::highlighting::Highlighter::new(&self.light),
            dark: syntect::highlighting::Highlighter::new(&self.dark),
            failed: false,
        }
    }
}

/// Highlights lines one by one, carrying parser state across calls so that
/// multi-line constructs such as block comments stay colored.
pub struct LineHighlighter<'a> {
    owner: &'a Highlighter,
    state: ParseState,
    stack: ScopeStack,
    light: syntect::highlighting::Highlighter<'a>,
    dark: syntect::highlighting::Highlighter<'a>,
    failed: bool,
}

impl LineHighlighter<'_> {
    /// Each returned line is self-contained HTML: no span stays open across lines.
    pub fn line(&mut self, text: &str) -> String {
        let mut out = String::new();
        if self.failed {
            escape_into(&mut out, text);
            return out;
        }
        let with_newline = format!("{text}\n");
        let ops = match self.state.parse_line(&with_newline, &self.owner.syntaxes) {
            Ok(ops) => ops,
            Err(_) => {
                self.failed = true;
                escape_into(&mut out, text);
                return out;
            }
        };
        let mut last = 0;
        for (idx, op) in ops {
            let idx = idx.min(text.len());
            if idx > last {
                self.emit(&mut out, &text[last..idx]);
                last = idx;
            }
            if self.stack.apply(&op).is_err() {
                self.failed = true;
            }
        }
        if last < text.len() {
            self.emit(&mut out, &text[last..]);
        }
        out
    }

    fn emit(&self, out: &mut String, text: &str) {
        let scopes = self.stack.as_slice();
        let light = self.light.style_for_stack(scopes);
        let dark = self.dark.style_for_stack(scopes);
        let light_default = self.owner.light.settings.foreground;
        let dark_default = self.owner.dark.settings.foreground;
        let plain = Some(light.foreground) == light_default
            && Some(dark.foreground) == dark_default
            && light.font_style.is_empty();
        if plain {
            escape_into(out, text);
            return;
        }
        out.push_str("<span style=\"--l:");
        out.push_str(&hex(light.foreground));
        out.push_str(";--d:");
        out.push_str(&hex(dark.foreground));
        if light.font_style.contains(FontStyle::BOLD) {
            out.push_str(";font-weight:600");
        }
        if light.font_style.contains(FontStyle::ITALIC) {
            out.push_str(";font-style:italic");
        }
        out.push_str("\">");
        escape_into(out, text);
        out.push_str("</span>");
    }
}

fn hex(c: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

pub fn escape_into(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_keywords_and_escapes_html() {
        let h = highlighter();
        let mut lines = h.lines(h.syntax_for_path("src/main.rs"));
        let html = lines.line("fn main() { let a = 1 < 2; }");
        assert!(html.contains("<span style=\"--l:#"), "{html}");
        assert!(html.contains("&lt;"));
        assert!(!html.contains("1 < 2"));
    }

    #[test]
    fn carries_state_across_lines() {
        let h = highlighter();
        let mut lines = h.lines(h.syntax_for_path("a.rs"));
        let first = lines.line("/* start");
        let second = lines.line("still comment */");
        assert!(first.starts_with("<span"));
        assert!(second.starts_with("<span"), "{second}");
    }

    #[test]
    fn unknown_files_are_plain_but_escaped() {
        let h = highlighter();
        let mut lines = h.lines(h.syntax_for_path("unknown.zzz"));
        assert_eq!(lines.line("<b>"), "&lt;b&gt;");
    }

    #[test]
    fn finds_syntax_by_file_name() {
        let h = highlighter();
        assert_eq!(h.syntax_for_path("Makefile").name, "Makefile");
        assert_eq!(h.syntax_for_lang("ts").name, "TypeScript");
    }
}
