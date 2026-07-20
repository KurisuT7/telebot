use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddrV4, SocketAddrV6};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use grammers_session::SessionData;
use grammers_session::storages::SqliteSession;
use serde::Deserialize;

#[derive(Deserialize)]
struct TeleboxConfig {
    session: String,
}

struct GramJsSession {
    dc_id: i32,
    address: IpAddr,
    port: u16,
    auth_key: [u8; 256],
}

pub async fn import_gramjs_config(source: &Path, destination: &Path) -> Result<()> {
    if destination.exists() {
        bail!(
            "destination session {} already exists; refusing to overwrite it",
            destination.display()
        );
    }
    let raw = fs::read_to_string(source)
        .with_context(|| format!("failed to read {}", source.display()))?;
    let config: TeleboxConfig = serde_json::from_str(&raw)
        .with_context(|| format!("invalid TeleBox config {}", source.display()))?;
    let imported = parse_gramjs_session(config.session.trim())?;

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let session = SqliteSession::open(destination).await?;
    let mut data = SessionData {
        home_dc: imported.dc_id,
        ..Default::default()
    };
    let dc = data
        .dc_options
        .get_mut(&imported.dc_id)
        .ok_or_else(|| anyhow!("unsupported Telegram DC {}", imported.dc_id))?;
    match imported.address {
        IpAddr::V4(address) => dc.ipv4 = SocketAddrV4::new(address, imported.port),
        IpAddr::V6(address) => dc.ipv6 = SocketAddrV6::new(address, imported.port, 0, 0),
    }
    dc.auth_key = Some(imported.auth_key);
    data.import_to(&session).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o600))?;
    }
    println!(
        "Imported Telegram authorization into DC {} session",
        imported.dc_id
    );
    Ok(())
}

fn parse_gramjs_session(value: &str) -> Result<GramJsSession> {
    let encoded = value
        .strip_prefix('1')
        .ok_or_else(|| anyhow!("unsupported GramJS StringSession version"))?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("invalid GramJS StringSession base64")?;
    if decoded.len() < 1 + 4 + 2 + 256 {
        bail!("GramJS StringSession is too short");
    }

    let dc_id = decoded[0] as i32;
    let mut offset = 1usize;
    let address = if encoded.len() == 352 {
        let octets: [u8; 4] = decoded[offset..offset + 4].try_into().unwrap();
        offset += 4;
        IpAddr::V4(Ipv4Addr::from(octets))
    } else {
        let address_len =
            u16::from_be_bytes(decoded[offset..offset + 2].try_into().unwrap()) as usize;
        offset += 2;
        if address_len == 0 || address_len > 64 || offset + address_len + 2 + 256 != decoded.len() {
            bail!("invalid GramJS server address length");
        }
        let address = std::str::from_utf8(&decoded[offset..offset + address_len])
            .context("GramJS server address is not UTF-8")?
            .parse::<IpAddr>()
            .context("GramJS server address is not an IP address")?;
        offset += address_len;
        address
    };
    let port = u16::from_be_bytes(decoded[offset..offset + 2].try_into().unwrap());
    offset += 2;
    let auth_key: [u8; 256] = decoded[offset..]
        .try_into()
        .map_err(|_| anyhow!("GramJS authorization key must be 256 bytes"))?;
    if dc_id == 0 || port == 0 {
        bail!("GramJS session contains an invalid DC or port");
    }
    Ok(GramJsSession {
        dc_id,
        address,
        port,
        auth_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gramjs_ipv4_session() {
        let mut raw = vec![5_u8];
        let address = b"91.108.56.130";
        raw.extend_from_slice(&(address.len() as u16).to_be_bytes());
        raw.extend_from_slice(address);
        raw.extend_from_slice(&443_u16.to_be_bytes());
        raw.extend_from_slice(&[7_u8; 256]);
        let encoded = format!("1{}", base64::engine::general_purpose::STANDARD.encode(raw));
        let parsed = parse_gramjs_session(&encoded).unwrap();
        assert_eq!(parsed.dc_id, 5);
        assert_eq!(parsed.port, 443);
        assert_eq!(parsed.address, "91.108.56.130".parse::<IpAddr>().unwrap());
        assert_eq!(parsed.auth_key, [7_u8; 256]);
    }
}
