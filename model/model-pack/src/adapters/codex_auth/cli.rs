//! Management command of the provider executable, outside the model wire protocol.
use std::{ffi::OsString, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use super::{
    ISSUER, default_auth_file, login,
    oauth::{OAuthClient, now},
    store,
};

#[derive(Parser)]
#[command(
    name = "proteus-model-pack-module auth openai_codex",
    bin_name = "proteus-model-pack-module auth openai_codex",
    about = "ChatGPT subscription authentication for Proteus"
)]
struct Cli {
    #[arg(long, global = true, help = "Separate Proteus credential file")]
    auth_file: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        help = "Machine-readable result without credentials"
    )]
    json: bool,
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Sign in with ChatGPT in the browser or using a device code.
    Login {
        #[arg(long)]
        device_auth: bool,
        #[arg(long, conflicts_with = "device_auth")]
        no_browser: bool,
    },
    /// Show local login state without printing tokens or refreshing them.
    Status,
    /// Remove this Proteus login locally.
    Logout,
}

pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<()> {
    let cli = Cli::parse_from(args);
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            tokio::select! {
                result = execute(cli) => result,
                _ = tokio::signal::ctrl_c() => bail!("ChatGPT auth command canceled"),
            }
        })
}

async fn execute(cli: Cli) -> Result<()> {
    let path = cli.auth_file.map(Ok).unwrap_or_else(default_auth_file)?;
    let _lock = tokio::time::timeout(Duration::from_secs(45), store::lock(&path))
        .await
        .context("another ChatGPT auth operation is still running")??;
    match cli.action {
        Action::Login {
            device_auth,
            no_browser,
        } => {
            let oauth = OAuthClient::new(ISSUER)?;
            let credentials = tokio::time::timeout(Duration::from_secs(15 * 60), async {
                if device_auth {
                    login::device(&oauth).await
                } else {
                    login::browser(&oauth, !no_browser).await
                }
            })
            .await
            .context("ChatGPT login timed out after 15 minutes")??;
            store::write(&path, &credentials)?;
            if cli.json {
                println!("{}", serde_json::json!({"authenticated": true}));
            } else {
                println!("Вход ChatGPT сохранён: {}", path.display());
            }
        }
        Action::Status if cli.json => {
            println!("{}", status_value(&path)?);
        }
        Action::Status => match store::read(&path)? {
            Some(credential) => {
                let state = if credential.expires_at > now() {
                    "активен"
                } else {
                    "требует обновления при следующем запросе"
                };
                println!(
                    "ChatGPT subscription: вход сохранён; access token {state}.\nФайл: {}",
                    path.display()
                );
            }
            None => println!(
                "ChatGPT subscription: вход не выполнен.\nФайл: {}",
                path.display()
            ),
        },
        Action::Logout => {
            store::remove(&path)?;
            println!("Локальный вход ChatGPT удалён: {}", path.display());
        }
    }
    Ok(())
}

fn status_value(path: &std::path::Path) -> Result<serde_json::Value> {
    let credential = store::read(path)?;
    Ok(serde_json::json!({
        "authenticated": credential.is_some(),
        "refresh_required": credential.as_ref().is_some_and(|value| value.expires_at <= now()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_status_reports_presence_without_returning_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("credentials.json");
        assert_eq!(
            status_value(&path).unwrap(),
            serde_json::json!({
                "authenticated": false, "refresh_required": false,
            })
        );
        let credentials = super::super::store::Credentials {
            access_token: "synthetic-access".into(),
            refresh_token: "synthetic-refresh".into(),
            account_id: "synthetic-account".into(),
            expires_at: now() + 3600,
        };
        store::write(&path, &credentials).unwrap();
        let status = status_value(&path).unwrap();
        assert_eq!(
            status,
            serde_json::json!({
                "authenticated": true, "refresh_required": false,
            })
        );
        assert!(!status.to_string().contains("synthetic"));
    }
}
