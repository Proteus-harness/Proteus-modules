use std::{
    borrow::Cow,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};

const ROOT_WORKSPACE_DIR: &str = "%2F";

pub fn encode_workspace_path(path: &Path) -> Result<String> {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !path.is_absolute() {
        bail!("workspace path must be absolute: {}", path.display());
    }

    // With no escapes this is at most the length of the original path.
    // Build the directory name directly instead of allocating each component.
    let mut encoded = String::new();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => {
                let part = part.to_str().ok_or_else(|| {
                    anyhow!(
                        "workspace path contains a non-UTF-8 component: {}",
                        path.display()
                    )
                })?;
                if encoded.is_empty() {
                    // A raw path used after canonicalization fails may dwarf
                    // its encoded components (for example repeated separators).
                    encoded.reserve_exact(path.as_os_str().len().min(256));
                } else {
                    encoded.push('|');
                }
                append_encoded_workspace_component(&mut encoded, part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) => {
                bail!("unsupported workspace path: {}", path.display());
            }
        }
    }

    if encoded.is_empty() {
        Ok(ROOT_WORKSPACE_DIR.to_owned())
    } else {
        Ok(encoded)
    }
}

pub fn decode_workspace_path(encoded: &str) -> Result<PathBuf> {
    let path = if encoded == ROOT_WORKSPACE_DIR {
        PathBuf::from("/")
    } else {
        if encoded.is_empty() {
            bail!("encoded workspace directory name must not be empty");
        }

        let mut path = PathBuf::from("/");
        for encoded_part in encoded.split('|') {
            let part = decode_workspace_component(encoded_part)?;
            let part = part.as_ref();
            let mut components = Path::new(part).components();
            if !matches!(components.next(), Some(Component::Normal(_)))
                || components.next().is_some()
            {
                bail!("invalid encoded workspace path component {encoded_part:?}");
            }
            path.push(part);
        }
        path
    };

    let canonical = std::fs::canonicalize(&path)
        .with_context(|| format!("encoded workspace path does not exist: {}", path.display()))?;
    let canonical_encoded = encode_workspace_path(&canonical)?;
    if canonical_encoded != encoded {
        bail!(
            "workspace directory name {encoded:?} is not canonical for {} (expected {canonical_encoded:?})",
            canonical.display()
        );
    }
    Ok(canonical)
}

pub(crate) fn workspace_path_from_session_dir(session_dir: &Path) -> Result<PathBuf> {
    let workspace_dir = session_dir.parent().ok_or_else(|| {
        anyhow!(
            "session directory has no workspace parent: {}",
            session_dir.display()
        )
    })?;
    let encoded = workspace_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            anyhow!(
                "session workspace directory must have a UTF-8 name: {}",
                workspace_dir.display()
            )
        })?;
    decode_workspace_path(encoded)
}

fn unescaped_workspace_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.')
}

fn append_encoded_workspace_component(out: &mut String, input: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for ch in input.chars() {
        if unescaped_workspace_char(ch) {
            out.push(ch);
        } else {
            let mut bytes = [0_u8; 4];
            for byte in ch.encode_utf8(&mut bytes).as_bytes() {
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
}

fn decode_workspace_component(encoded: &str) -> Result<Cow<'_, str>> {
    if encoded.is_empty() {
        bail!("encoded workspace path contains an empty component");
    }

    let input = encoded.as_bytes();
    // With no escapes, the input is already UTF-8. Use the encoder's
    // predicate to validate canonical spelling without a temporary buffer.
    if encoded.chars().all(unescaped_workspace_char) {
        return Ok(Cow::Borrowed(encoded));
    }

    let mut decoded = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] != b'%' {
            decoded.push(input[index]);
            index += 1;
            continue;
        }
        if index + 2 >= input.len() {
            bail!("invalid percent escape in workspace component {encoded:?}");
        }
        let high = decode_hex(input[index + 1])
            .ok_or_else(|| anyhow!("invalid percent escape in workspace component {encoded:?}"))?;
        let low = decode_hex(input[index + 2])
            .ok_or_else(|| anyhow!("invalid percent escape in workspace component {encoded:?}"))?;
        decoded.push((high << 4) | low);
        index += 3;
    }

    let decoded = String::from_utf8(decoded)
        .with_context(|| format!("workspace component is not valid UTF-8: {encoded:?}"))?;
    let mut canonical = String::new();
    append_encoded_workspace_component(&mut canonical, &decoded);
    if canonical != encoded {
        bail!("workspace component is not canonically encoded: {encoded:?}");
    }
    Ok(Cow::Owned(decoded))
}

fn decode_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn workspace_encoding_preserves_canonical_name_for_escapes_and_unicode() {
        let parent = tempfile::tempdir().unwrap();
        let base = encode_workspace_path(parent.path()).unwrap();
        let real = parent
            .path()
            .join("ordinary.alpha")
            .join("исследование_日本語")
            .join("percent% | 🤖 room");
        std::fs::create_dir_all(&real).unwrap();
        let exact = format!(
            "{base}|ordinary.alpha|исследование_日本語|percent%25%20%7C%20%F0%9F%A4%96%20room"
        );
        assert_eq!(encode_workspace_path(&real).unwrap(), exact);
        assert_eq!(
            decode_workspace_path(&exact).unwrap(),
            std::fs::canonicalize(&real).unwrap(),
        );
        let alias = parent.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        assert_eq!(encode_workspace_path(&alias).unwrap(), exact);
        assert_eq!(encode_workspace_path(Path::new("/")).unwrap(), "%2F");
        assert_eq!(decode_workspace_path("%2F").unwrap(), Path::new("/"));
    }
}
