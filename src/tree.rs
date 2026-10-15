use std::collections::BTreeMap;

#[derive(Debug, PartialEq, Eq)]
pub enum TreeNode {
    Dir {
        name: String,
        children: Vec<TreeNode>,
    },
    /// `index` points back into the slice passed to [`build`].
    File { name: String, index: usize },
}

/// Builds a directory tree like GitHub's file browser: directories before
/// files, and chains of single-child directories collapsed into `a/b/c`.
pub fn build<S: AsRef<str>>(paths: &[S]) -> Vec<TreeNode> {
    #[derive(Default)]
    struct Dir {
        dirs: BTreeMap<String, Dir>,
        files: Vec<(String, usize)>,
    }

    let mut root = Dir::default();
    for (index, path) in paths.iter().enumerate() {
        let mut parts: Vec<&str> = path.as_ref().split('/').collect();
        let file = parts.pop().unwrap_or_default();
        let mut dir = &mut root;
        for part in parts {
            dir = dir.dirs.entry(part.to_string()).or_default();
        }
        dir.files.push((file.to_string(), index));
    }

    fn convert(dir: Dir) -> Vec<TreeNode> {
        let mut nodes = Vec::new();
        for (mut name, mut sub) in dir.dirs {
            while sub.files.is_empty() && sub.dirs.len() == 1 {
                let (child_name, child) = sub.dirs.into_iter().next().expect("one child");
                name = format!("{name}/{child_name}");
                sub = child;
            }
            nodes.push(TreeNode::Dir {
                name,
                children: convert(sub),
            });
        }
        let mut files = dir.files;
        files.sort();
        nodes.extend(
            files
                .into_iter()
                .map(|(name, index)| TreeNode::File { name, index }),
        );
        nodes
    }
    convert(root)
}

/// File indexes in the order the tree displays them.
pub fn file_order(nodes: &[TreeNode]) -> Vec<usize> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            TreeNode::Dir { children, .. } => out.extend(file_order(children)),
            TreeNode::File { index, .. } => out.push(*index),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, index: usize) -> TreeNode {
        TreeNode::File {
            name: name.into(),
            index,
        }
    }

    #[test]
    fn puts_directories_first_and_collapses_single_child_chains() {
        let tree = build(&["z.txt", "src/a/b/c.rs", "src/main.rs", "a.txt"]);
        assert_eq!(
            tree,
            vec![
                TreeNode::Dir {
                    name: "src".into(),
                    children: vec![
                        TreeNode::Dir {
                            name: "a/b".into(),
                            children: vec![file("c.rs", 1)]
                        },
                        file("main.rs", 2),
                    ],
                },
                file("a.txt", 3),
                file("z.txt", 0),
            ]
        );
        assert_eq!(file_order(&tree), vec![1, 2, 3, 0]);
    }
}
