use std::collections::HashSet;
use std::path::Path;

use ruff_python_ast::*;

use super::LiteralKey;

/// Extract a human-readable name from an expression (for debugging / attr
/// chain resolution).
pub(super) fn get_ast_node_name(expr: &Expr) -> String {
    match expr {
        Expr::Name(n) => n.id.to_string(),
        Expr::Attribute(a) => {
            format!("{}.{}", get_ast_node_name(&a.value), a.attr.id)
        }
        _ => String::new(),
    }
}

/// Collect all Name identifiers from an assignment target expression.
pub(super) fn collect_target_names_from_expr(target: &Expr, names: &mut HashSet<String>) {
    match target {
        Expr::Name(n) => {
            names.insert(n.id.to_string());
        }
        Expr::Tuple(t) => {
            for elt in &t.elts {
                collect_target_names_from_expr(elt, names);
            }
        }
        Expr::List(l) => {
            for elt in &l.elts {
                collect_target_names_from_expr(elt, names);
            }
        }
        Expr::Starred(s) => {
            collect_target_names_from_expr(&s.value, names);
        }
        _ => {}
    }
}

/// Extract a small literal key we can use for shallow subscript resolution.
pub(super) fn literal_key_from_expr(expr: &Expr) -> Option<LiteralKey> {
    match expr {
        Expr::StringLiteral(s) => Some(LiteralKey::String(s.value.to_str().to_string())),
        Expr::NumberLiteral(n) => match &n.value {
            Number::Int(i) => i.as_i64().map(LiteralKey::Int),
            Number::Float(_) | Number::Complex { .. } => None,
        },
        Expr::UnaryOp(u) if matches!(u.op, UnaryOp::USub) => {
            if let Expr::NumberLiteral(n) = u.operand.as_ref() {
                if let Number::Int(i) = &n.value {
                    return i.as_i64().and_then(|value| value.checked_neg()).map(LiteralKey::Int);
                }
            }
            None
        }
        _ => None,
    }
}

/// Convert a Python source filename to a dotted module name.
///
/// If `root` is `None`, walks up directories checking for `__init__.py` to
/// find the package root.
pub fn get_module_name(filename: &str, root: Option<&str>) -> String {
    let path = Path::new(filename);

    // Determine the module path (without .py extension)
    let module_path_buf;
    let module_path: &Path = if path.file_name().is_some_and(|f| f == "__init__.py") {
        path.parent().unwrap_or(path)
    } else {
        module_path_buf = path.with_extension("");
        &module_path_buf
    };

    if let Some(root_dir) = root {
        // Root is known -- just strip it and join with dots
        let root_path = Path::new(root_dir);
        if let Ok(relative) = module_path.strip_prefix(root_path) {
            return relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(".");
        }
    }

    // Walk up directories checking for __init__.py
    let mut directories: Vec<(std::path::PathBuf, bool)> =
        vec![(module_path.to_path_buf(), true)];

    let mut current = module_path.parent();
    while let Some(dir) = current {
        if dir == Path::new("") || dir == Path::new("/") {
            break;
        }
        let has_init = dir.join("__init__.py").exists();
        directories.insert(0, (dir.to_path_buf(), has_init));
        if !has_init {
            break;
        }
        current = dir.parent();
    }

    // Keep only from the first directory that is a package root
    while directories.len() > 1 && !directories[0].1 {
        directories.remove(0);
    }

    directories
        .iter()
        .map(|(p, _)| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_module_name_simple() {
        // Without package structure, just the filename stem
        let name = get_module_name("foo.py", None);
        assert!(name.ends_with("foo"), "got: {name}");
    }

    #[test]
    fn test_get_module_name_init() {
        // __init__.py should use directory name
        let name = get_module_name("pkg/__init__.py", Some(""));
        assert!(name.ends_with("pkg"), "got: {name}");
    }

    #[test]
    fn test_get_ast_node_name() {
        // Just a basic smoke test -- the real tests happen via integration.
        assert_eq!(
            get_ast_node_name(&Expr::Name(ExprName {
                node_index: AtomicNodeIndex::default(),
                range: ruff_text_size::TextRange::default(),
                id: "foo".into(),
                ctx: ExprContext::Load,
            })),
            "foo"
        );
    }
}
