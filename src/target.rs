use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    MarkdownFile { path: PathBuf },
    MarkdownDir { path: PathBuf },
    Diff { repo: PathBuf, spec: DiffSpec },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiffSpec {
    WorkingTree,
    /// `from...to`: changes on `to` since it forked from `from`, like a pull request.
    MergeBase {
        from: String,
        to: String,
    },
    /// `from..to`: the trees of the two refs compared directly.
    Range {
        from: String,
        to: String,
    },
    Commit {
        rev: String,
    },
}

impl DiffSpec {
    pub fn parse(
        spec: Option<&str>,
        base: Option<&str>,
        default_base: impl FnOnce() -> anyhow::Result<String>,
    ) -> anyhow::Result<DiffSpec> {
        let Some(spec) = spec else {
            return Ok(DiffSpec::WorkingTree);
        };
        let side = |s: &str| {
            if s.is_empty() {
                "HEAD".to_string()
            } else {
                s.to_string()
            }
        };
        if let Some((from, to)) = spec.split_once("...") {
            return Ok(DiffSpec::MergeBase {
                from: side(from),
                to: side(to),
            });
        }
        if let Some((from, to)) = spec.split_once("..") {
            return Ok(DiffSpec::Range {
                from: side(from),
                to: side(to),
            });
        }
        let from = match base {
            Some(base) => base.to_string(),
            None => default_base()?,
        };
        Ok(DiffSpec::MergeBase {
            from,
            to: spec.to_string(),
        })
    }

    pub fn revisions(&self) -> Vec<&str> {
        match self {
            DiffSpec::WorkingTree => vec![],
            DiffSpec::MergeBase { from, to } | DiffSpec::Range { from, to } => vec![from, to],
            DiffSpec::Commit { rev } => vec![rev],
        }
    }

    pub fn title(&self) -> String {
        match self {
            DiffSpec::WorkingTree => "Uncommitted changes".to_string(),
            DiffSpec::MergeBase { from, to } => format!("{from}...{to}"),
            DiffSpec::Range { from, to } => format!("{from}..{to}"),
            DiffSpec::Commit { rev } => rev.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(spec: Option<&str>, base: Option<&str>) -> DiffSpec {
        DiffSpec::parse(spec, base, || Ok("main".into())).unwrap()
    }

    #[test]
    fn no_spec_means_working_tree() {
        assert_eq!(parse(None, None), DiffSpec::WorkingTree);
    }

    #[test]
    fn three_dots_compare_from_merge_base() {
        assert_eq!(
            parse(Some("main...feat"), None),
            DiffSpec::MergeBase {
                from: "main".into(),
                to: "feat".into()
            }
        );
    }

    #[test]
    fn two_dots_compare_trees_and_empty_side_is_head() {
        assert_eq!(
            parse(Some("v1.0.."), None),
            DiffSpec::Range {
                from: "v1.0".into(),
                to: "HEAD".into()
            }
        );
    }

    #[test]
    fn single_branch_uses_given_base() {
        assert_eq!(
            parse(Some("feat"), Some("develop")),
            DiffSpec::MergeBase {
                from: "develop".into(),
                to: "feat".into()
            }
        );
    }

    #[test]
    fn single_branch_without_base_uses_default_branch() {
        assert_eq!(
            parse(Some("feat"), None),
            DiffSpec::MergeBase {
                from: "main".into(),
                to: "feat".into()
            }
        );
    }
}
