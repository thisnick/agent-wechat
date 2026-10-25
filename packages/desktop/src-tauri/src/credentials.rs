use anyhow::{Context, Result};
use zeroize::Zeroizing;
const SERVICE: &str = "io.github.thisnick.agent-wechat";

pub fn read(installation: &str, connection: &str) -> Result<Option<Zeroizing<String>>> {
    let entry = keyring::Entry::new(SERVICE, &format!("{installation}:{connection}"))
        .map_err(|_| anyhow::anyhow!("The system keyring is unavailable"))?;
    match entry.get_password() {
        Ok(value) => {
            super::config::token(&value)?;
            Ok(Some(Zeroizing::new(value)))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err(anyhow::anyhow!(
            "Unlock your system keyring and try again; no credentials were changed"
        )),
    }
}
pub fn write(installation: &str, connection: &str, value: &str) -> Result<()> {
    super::config::token(value)?;
    keyring::Entry::new(SERVICE, &format!("{installation}:{connection}"))
        .map_err(|_| anyhow::anyhow!("The system keyring is unavailable"))?
        .set_password(value)
        .map_err(|_| anyhow::anyhow!("Could not save the access key in the system keyring"))?;
    let saved = read(installation, connection)?.context("Could not verify the saved access key")?;
    if saved.as_str() != value {
        anyhow::bail!("Could not verify the saved access key");
    }
    Ok(())
}
pub fn remove(installation: &str, connection: &str) -> Result<()> {
    let entry = keyring::Entry::new(SERVICE, &format!("{installation}:{connection}"))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err(anyhow::anyhow!("Could not remove the saved access key")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires an isolated, unlocked system credential store"]
    fn system_keyring_roundtrip() {
        let installation = uuid::Uuid::new_v4().to_string();
        let key = crate::hub::fresh_key();
        assert!(read(&installation, "fixture").unwrap().is_none());
        write(&installation, "fixture", &key).unwrap();
        assert_eq!(
            read(&installation, "fixture").unwrap().unwrap().as_str(),
            key.as_str()
        );
        remove(&installation, "fixture").unwrap();
        assert!(read(&installation, "fixture").unwrap().is_none());
    }
}
