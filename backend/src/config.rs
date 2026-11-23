use std::env;

use anyhow::{bail, Context, Result};
use base64::Engine;

#[derive(Clone, Debug)]
pub struct PlaidConfig {
    pub client_id: String,
    pub secret: String,
    pub base_url: String,
    pub redirect_uri: Option<String>,
}

#[derive(Clone, Debug)]
pub struct MailConfig {
    /// e.g. `smtps://user:pass@smtp.example.com:465` or `smtp://host:587?tls=required`
    pub smtp_url: String,
    pub from: String,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub bind_addr: String,
    pub static_dir: String,
    pub catalog_dir: String,
    pub encryption_key: [u8; 32],
    pub admin_email: Option<String>,
    pub admin_password: Option<String>,
    pub require_mfa: bool,
    pub cookie_secure: bool,
    pub plaid: Option<PlaidConfig>,
    pub mail: Option<MailConfig>,
    /// Local hour (user timezone) of the nightly transaction sync.
    pub sync_hour: u32,
    /// Local hour (user timezone) of the daily alert run.
    pub alert_hour: u32,
    /// Set to false to run the API without background jobs (tests).
    pub run_scheduler: bool,
}

fn opt(name: &str) -> Option<String> {
    env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn flag(name: &str, default: bool) -> bool {
    match opt(name).as_deref() {
        Some("1" | "true" | "yes" | "on") => true,
        Some("0" | "false" | "no" | "off") => false,
        _ => default,
    }
}

pub fn parse_key(b64: &str) -> Result<[u8; 32]> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .context("APP_ENCRYPTION_KEY must be base64")?;
    let key: [u8; 32] = raw
        .try_into()
        .map_err(|_| anyhow::anyhow!("APP_ENCRYPTION_KEY must decode to exactly 32 bytes (openssl rand -base64 32)"))?;
    Ok(key)
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let database_url = opt("DATABASE_URL").context("DATABASE_URL is required")?;
        let encryption_key = parse_key(&opt("APP_ENCRYPTION_KEY").context("APP_ENCRYPTION_KEY is required")?)?;

        let plaid = match (opt("PLAID_CLIENT_ID"), opt("PLAID_SECRET")) {
            (Some(client_id), Some(secret)) => {
                let base_url = match opt("PLAID_BASE_URL") {
                    Some(u) => u.trim_end_matches('/').to_string(),
                    None => match opt("PLAID_ENV").as_deref().unwrap_or("sandbox") {
                        "sandbox" => "https://sandbox.plaid.com".to_string(),
                        "production" => "https://production.plaid.com".to_string(),
                        other => bail!("PLAID_ENV must be sandbox or production, got {other}"),
                    },
                };
                Some(PlaidConfig { client_id, secret, base_url, redirect_uri: opt("PLAID_REDIRECT_URI") })
            }
            (None, None) => None,
            _ => bail!("set both PLAID_CLIENT_ID and PLAID_SECRET, or neither"),
        };

        let mail = match (opt("SMTP_URL"), opt("MAIL_FROM")) {
            (Some(smtp_url), Some(from)) => Some(MailConfig { smtp_url, from }),
            (None, _) => None,
            (Some(_), None) => bail!("MAIL_FROM is required when SMTP_URL is set"),
        };

        let hour = |name: &str, default: u32| -> Result<u32> {
            match opt(name) {
                None => Ok(default),
                Some(v) => {
                    let h: u32 = v.parse().with_context(|| format!("{name} must be an hour 0-23"))?;
                    if h > 23 {
                        bail!("{name} must be an hour 0-23");
                    }
                    Ok(h)
                }
            }
        };

        Ok(Config {
            database_url,
            bind_addr: opt("BIND_ADDR").unwrap_or_else(|| "0.0.0.0:8080".into()),
            static_dir: opt("STATIC_DIR").unwrap_or_else(|| "./static".into()),
            catalog_dir: opt("CATALOG_DIR").unwrap_or_else(|| "./catalog".into()),
            encryption_key,
            admin_email: opt("ADMIN_EMAIL"),
            admin_password: opt("ADMIN_PASSWORD"),
            require_mfa: flag("REQUIRE_MFA", true),
            cookie_secure: flag("COOKIE_SECURE", false),
            plaid,
            mail,
            sync_hour: hour("SYNC_HOUR", 3)?,
            alert_hour: hour("ALERT_HOUR", 8)?,
            run_scheduler: flag("RUN_SCHEDULER", true),
        })
    }
}
