use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use grammers_client::{Client, SignInError};
use grammers_mtsender::SenderPool;
use grammers_session::storages::SqliteSession;

use crate::config::Config;

pub async fn login(config_path: &Path) -> Result<()> {
    let config = Config::load(config_path)?;
    let api_hash = config.load_telegram_api_hash()?;
    prepare_session_path(&config.telegram.session_path).await?;
    restrict_session_files(&config.telegram.session_path).await?;

    let session = Arc::new(SqliteSession::open(&config.telegram.session_path).await?);
    restrict_session_files(&config.telegram.session_path).await?;
    let SenderPool { runner, handle, .. } =
        SenderPool::new(Arc::clone(&session), config.telegram.api_id);
    let client = Client::new(handle.clone());
    let pool_task = tokio::spawn(runner.run());

    let result = authorize(&client, &api_hash).await;
    handle.quit();
    let _ = pool_task.await;
    restrict_session_files(&config.telegram.session_path).await?;
    result
}

async fn authorize(client: &Client, api_hash: &str) -> Result<()> {
    if client.is_authorized().await? {
        println!("Telegram session is already authorized");
        return Ok(());
    }

    let phone = prompt_line("Phone number in international format: ", 64)?;
    let token = client
        .request_login_code(&phone, api_hash)
        .await
        .context("failed to request Telegram login code")?;
    let code = prompt_secret("Login code: ", 32)?;

    match client.sign_in(&token, &code).await {
        Ok(_) => {}
        Err(SignInError::PasswordRequired(password_token)) => {
            let prompt = password_prompt(password_token.hint());
            let password = prompt_secret(&prompt, 1024)?;
            match client.check_password(password_token, password).await {
                Ok(_) => {}
                Err(SignInError::InvalidPassword(_)) => {
                    bail!("Telegram 2FA password was invalid; run login again")
                }
                Err(error) => return Err(error).context("Telegram 2FA login failed"),
            }
        }
        Err(SignInError::InvalidCode) => {
            bail!("Telegram login code was invalid; run login again")
        }
        Err(SignInError::SignUpRequired) => {
            bail!("create the Telegram account in an official client before running login")
        }
        Err(error) => return Err(error).context("Telegram login failed"),
    }

    if !client.is_authorized().await? {
        bail!("Telegram did not retain the new authorization")
    }
    println!("Telegram session authorized successfully");
    Ok(())
}

fn prompt_line(prompt: &str, max_chars: usize) -> Result<String> {
    print!("{prompt}");
    io::stdout()
        .flush()
        .context("failed to show login prompt")?;
    let mut value = String::new();
    let bytes = io::stdin()
        .read_line(&mut value)
        .context("failed to read login input")?;
    if bytes == 0 {
        bail!("login input was closed")
    }
    let value = value.trim().to_owned();
    if value.is_empty() {
        bail!("login input must not be empty")
    }
    if value.chars().count() > max_chars {
        bail!("login input is too long")
    }
    Ok(value)
}

fn prompt_secret(prompt: &str, max_chars: usize) -> Result<String> {
    let value = rpassword::prompt_password(prompt)
        .context("failed to read hidden login input from the terminal")?;
    if value.is_empty() {
        bail!("login input must not be empty")
    }
    if value.chars().count() > max_chars {
        bail!("login input is too long")
    }
    Ok(value)
}

fn password_prompt(hint: Option<&str>) -> String {
    let hint = hint
        .unwrap_or_default()
        .chars()
        .filter(|character| !character.is_control())
        .take(80)
        .collect::<String>();
    if hint.is_empty() {
        "Telegram 2FA password: ".to_owned()
    } else {
        format!("Telegram 2FA password (hint: {hint}): ")
    }
}

async fn prepare_session_path(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .context("Telegram session path must have a parent directory")?;
    #[cfg(unix)]
    let parent_existed = parent.exists();
    tokio::fs::create_dir_all(parent)
        .await
        .with_context(|| format!("failed to create {}", parent.display()))?;
    #[cfg(unix)]
    if !parent_existed {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).await?;
    }

    if !path.exists() {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(path)
            .with_context(|| format!("failed to create {}", path.display()))?;
    }
    Ok(())
}

async fn restrict_session_files(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for candidate in [
            path.to_path_buf(),
            path.with_extension("session-wal"),
            path.with_extension("session-shm"),
        ] {
            if candidate.exists() {
                tokio::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o600))
                    .await
                    .with_context(|| {
                        format!("failed to restrict permissions on {}", candidate.display())
                    })?;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::password_prompt;

    #[test]
    fn sanitizes_password_hint_before_printing_it() {
        assert_eq!(
            password_prompt(Some("pet\nname\u{1b}[31m")),
            "Telegram 2FA password (hint: petname[31m): "
        );
        assert_eq!(password_prompt(None), "Telegram 2FA password: ");
    }
}
