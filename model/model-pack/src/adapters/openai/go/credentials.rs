use super::super::super::secrets::expand_user_path;
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use std::{env, path::PathBuf};

pub(super) fn key(config: &Value) -> Result<String> {
    if let Some(key) = config.get("api_key") {
        return Ok(nonempty(key.as_str())
            .context("invalid explicit OpenCode API key")?
            .to_owned());
    }
    let database = match config.get("database_path") {
        Some(path) => {
            expand_user_path(nonempty(path.as_str()).context("invalid OpenCode database path")?)
        }
        None => {
            let dir = env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .or_else(|| {
                    env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
                })
                .context("OpenCode data directory unavailable")?;
            dir.join("opencode/opencode.db")
        }
    };
    if database
        .try_exists()
        .context("OpenCode credential database unavailable")?
    {
        let db = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .context("cannot open OpenCode credential database")?;
        let row:Option<String>=db.query_row(
            "SELECT value FROM credential WHERE integration_id=?1 ORDER BY active DESC, time_created DESC, id DESC LIMIT 1",
            ["opencode-go"],|row|row.get(0)).optional().context("cannot read OpenCode credential database")?;
        if let Some(raw) = row {
            let v: Value = serde_json::from_str(&raw)
                .map_err(|_| anyhow::anyhow!("invalid stored OpenCode credential"))?;
            ensure!(
                v["type"] == "key",
                "OpenCode Go requires a stored API-key credential"
            );
            return Ok(nonempty(v["key"].as_str())
                .context("empty stored OpenCode key")?
                .to_owned());
        }
    }
    if let Ok(key) = env::var("OPENCODE_API_KEY")
        && !key.trim().is_empty()
    {
        return Ok(key);
    }
    bail!("OpenCode Go credentials unavailable")
}
fn nonempty(value: Option<&str>) -> Option<&str> {
    value.filter(|s| !s.trim().is_empty())
}
