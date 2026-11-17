use anyhow::{Context, Result, bail};
use std::{
    io::ErrorKind,
    path::{Component, Path, PathBuf},
};

/// Resolve an existing ancestor without creating missing session directories.
/// The resulting identity is stable when those directories are materialized.
pub(crate) fn canonicalize_unmaterialized_path(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)
        .with_context(|| format!("failed to resolve absolute path {}", path.display()))?;
    for ancestor in absolute.ancestors() {
        match std::fs::canonicalize(ancestor) {
            Ok(mut canonical) => {
                let suffix = absolute.strip_prefix(ancestor)?;
                for component in suffix.components() {
                    match component {
                        Component::Normal(name) => canonical.push(name),
                        Component::ParentDir => {
                            canonical.pop();
                        }
                        Component::CurDir => {}
                        _ => bail!("unexpected absolute suffix in {}", absolute.display()),
                    }
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to canonicalize session ancestor {}",
                        ancestor.display()
                    )
                });
            }
        }
    }
    bail!(
        "no existing ancestor for session path {}",
        absolute.display()
    )
}
