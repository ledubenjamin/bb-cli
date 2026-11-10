//! Native credential storage and serialized profile mutations.

use super::{Credential, expires_soon};
use crate::{
    args::TokenType,
    config::{self, Store},
};
use anyhow::{Context as _, Result, anyhow};
use keyring_core::api::CredentialStoreApi;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

#[expect(clippy::map_err_ignore, reason = "Backend errors may contain secrets")]
fn native_store() -> Result<std::sync::Arc<impl CredentialStoreApi>> {
    #[cfg(target_os = "macos")]
    use apple_native_keyring_store::keychain::Store as NativeStore;
    #[cfg(target_os = "linux")]
    use dbus_secret_service_keyring_store::Store as NativeStore;
    #[cfg(target_os = "windows")]
    use windows_native_keyring_store::Store as NativeStore;
    NativeStore::new().map_err(|_| {
        anyhow!("cannot open the system keyring; use BB_TOKEN in headless environments")
    })
}

#[derive(Deserialize)]
struct StoredCredential {
    token: String,
    refresh_token: Option<String>,
    expires_at: Option<u64>,
    token_type: TokenType,
}

#[expect(clippy::map_err_ignore, reason = "Backend errors may contain secrets")]
fn entry(store: &Store, profile: &str) -> Result<keyring_core::Entry> {
    // Isolate test/development configuration directories from the user's production credentials.
    let namespace = store.directory().to_string_lossy();
    native_store()?
        .build(&format!("bb:{namespace}"), profile, None)
        .map_err(|_| {
            anyhow!("cannot access the system keyring; use BB_TOKEN in headless environments")
        })
}
#[expect(
    clippy::map_err_ignore,
    reason = "Keyring backend diagnostics are intentionally hidden at the authentication boundary"
)]
fn read(store: &Store, profile: &str) -> Result<Credential> {
    let value = entry(store, profile)
        .and_then(|entry| entry.get_password().map_err(Into::into))
        .map_err(|_| crate::app::Failure {
            code: "authentication_required",
            message: "no usable stored credential; run bb auth login or set BB_TOKEN".into(),
            exit: 4,
        })?;
    let value = SecretString::from(value);
    let stored: StoredCredential = serde_json::from_str(value.expose_secret())
        .context("invalid stored credential; run bb auth login")?;
    Ok(Credential {
        token: SecretString::from(stored.token),
        refresh_token: stored.refresh_token.map(SecretString::from),
        expires_at: stored.expires_at,
        token_type: stored.token_type,
    })
}
#[expect(clippy::map_err_ignore, reason = "Backend errors may contain secrets")]
fn save(store: &Store, profile: &str, credential: &Credential) -> Result<()> {
    #[derive(Serialize)]
    struct Record<'a> {
        token: &'a str,
        refresh_token: Option<&'a str>,
        expires_at: Option<u64>,
        token_type: TokenType,
    }
    let stored = Record {
        token: credential.token.expose_secret(),
        refresh_token: credential
            .refresh_token
            .as_ref()
            .map(secrecy::ExposeSecret::expose_secret),
        expires_at: credential.expires_at,
        token_type: credential.token_type,
    };
    let serialized = SecretString::from(serde_json::to_string(&stored)?);
    entry(store,profile)?.set_password(serialized.expose_secret())
        .map_err(|_| anyhow!("system keyring unavailable; credentials were not saved; use BB_TOKEN for this environment"))
}

// The vault boundary lets refresh/locking tests use memory without touching a user's keyring.
pub(super) trait Vault: Clone + Send + 'static {
    fn read(&self) -> Result<Credential>;
    fn save(&self, credential: &Credential) -> Result<()>;
    fn delete(&self) -> Result<()>;
}
#[derive(Clone)]
pub(super) struct KeyringVault {
    pub(super) store: Store,
    pub(super) profile: String,
}
impl Vault for KeyringVault {
    fn read(&self) -> Result<Credential> {
        read(&self.store, &self.profile)
    }
    fn save(&self, credential: &Credential) -> Result<()> {
        save(&self.store, &self.profile, credential)
    }
    fn delete(&self) -> Result<()> {
        match entry(&self.store, &self.profile)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(_) => Err(anyhow!(
                "cannot remove stored credentials from the system keyring"
            )),
        }
    }
}

/// Rechecks under the profile lock before rotating a refresh token.
/// The blocking writer owns the lock until persistence completes, even if its
/// async caller is cancelled. Login and logout acquire the same OS lock.
pub(super) async fn stored_credential<V, F, Fut>(
    vault: V,
    store: &Store,
    profile: &str,
    force: bool,
    renew: F,
) -> Result<Credential>
where
    V: Vault,
    F: Fn(Credential) -> Fut,
    Fut: std::future::Future<Output = Result<Credential>>,
{
    let initial_vault = vault.clone();
    let initial = tokio::task::spawn_blocking(move || initial_vault.read()).await??;
    if !force && !expires_soon(&initial)? {
        return Ok(initial);
    }
    let lock = store.lock_profile(profile).await?;
    let locked_vault = vault.clone();
    let latest = tokio::task::spawn_blocking(move || locked_vault.read()).await??;
    // Another bb process may have rotated the token while this process waited.
    if !force && !expires_soon(&latest)? {
        return Ok(latest);
    }
    let credential = renew(latest).await?;
    tokio::task::spawn_blocking(move || -> Result<_> {
        // Keep exclusion until persistence completes, including after caller cancellation.
        let _lock = lock;
        vault.save(&credential)?;
        Ok(credential)
    })
    .await?
}

/// Saves credentials and metadata under the shared profile lock.
pub(super) async fn save_profile<V: Vault>(
    vault: V,
    store: &Store,
    profile: &str,
    credential: Credential,
    meta: config::Profile,
) -> Result<()> {
    let name = profile.to_owned();
    store
        .update_profile(profile, move |config| {
            vault.save(&credential)?;
            config.profiles.insert(name.clone(), meta);
            config.active_profile = Some(name);
            config.active_context = None;
            Ok(())
        })
        .await
}

/// A refresh waiting behind logout must observe the missing credential.
pub(super) async fn delete_profile<V: Vault>(vault: V, store: &Store, profile: &str) -> Result<()> {
    let name = profile.to_owned();
    store
        .update_profile(profile, move |config| {
            vault.delete()?;
            config.profiles.remove(&name);
            if config.active_profile.as_deref() == Some(&name) {
                config.active_profile = None;
            }
            Ok(())
        })
        .await
}

#[cfg(all(test, windows))]
mod native_tests {
    use super::*;

    struct Cleanup(KeyringVault);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = self.0.delete();
        }
    }

    #[test]
    #[ignore = "writes a temporary entry to Windows Credential Manager; run explicitly in native CI"]
    fn native_windows_keyring_round_trip() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let vault = KeyringVault {
            store: Store::for_test(directory.path().to_owned()),
            profile: "native-test".into(),
        };
        let _cleanup = Cleanup(vault.clone());
        vault
            .read()
            .err()
            .context("unexpected credential after deletion or before save")?;
        let credential = Credential {
            token: SecretString::from("bb-native-test-placeholder"),
            refresh_token: None,
            expires_at: None,
            token_type: TokenType::Api,
        };
        vault.save(&credential)?;
        assert_eq!(
            vault.read()?.token.expose_secret(),
            credential.token.expose_secret()
        );
        vault.delete()?;
        vault
            .read()
            .err()
            .context("unexpected credential after deletion or before save")?;
        vault.delete()?;
        Ok(())
    }
}
