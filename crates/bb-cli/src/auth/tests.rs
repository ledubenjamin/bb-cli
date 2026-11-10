use super::oauth::{callback, refresh_at};
use super::*;
use anyhow::anyhow;
use std::sync::Condvar;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, header, method, path},
};

#[derive(Clone)]
struct MemoryVault(Arc<Mutex<Option<Credential>>>);
impl Vault for MemoryVault {
    fn read(&self) -> Result<Credential> {
        self.0
            .lock()
            .map_err(|error| anyhow!("{error}"))?
            .clone()
            .context("no stored credential")
    }
    fn delete(&self) -> Result<()> {
        *self.0.lock().map_err(|error| anyhow!("{error}"))? = None;
        Ok(())
    }
    fn save(&self, credential: &Credential) -> Result<()> {
        *self.0.lock().map_err(|error| anyhow!("{error}"))? = Some(credential.clone());
        Ok(())
    }
}

fn expired() -> Credential {
    Credential {
        token: SecretString::from("expired"),
        refresh_token: Some(SecretString::from("refresh-old")),
        expires_at: Some(1),
        token_type: TokenType::Oauth,
    }
}

#[tokio::test]
async fn refresh_rotates_tokens_using_consumer_authentication() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/token")).and(header("authorization","Basic Y2xpZW50OnNlY3JldA=="))
        .and(body_string_contains("grant_type=refresh_token")).and(body_string_contains("refresh_token=refresh-old"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"access_token":"fresh","refresh_token":"refresh-new","token_type":"bearer","expires_in":3600}))).expect(1).mount(&server).await;
    let result = refresh_at(
        &expired(),
        "client".into(),
        SecretString::from("secret"),
        &format!("{}/token", server.uri()),
    )
    .await?;
    assert_eq!(result.token.expose_secret(), "fresh");
    assert_eq!(
        result
            .refresh_token
            .as_ref()
            .context("missing rotated refresh token")?
            .expose_secret(),
        "refresh-new"
    );
    assert!(!expires_soon(&result)?);
    assert!(!format!("{result:?}").contains("fresh"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_refreshes_recheck_after_acquiring_the_os_lock() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::for_test(directory.path().to_owned());
    let vault = MemoryVault(Arc::new(Mutex::new(Some(expired()))));
    let counter = Arc::new(AtomicUsize::new(0));
    let renew = |_: Credential| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok(Credential {
                token: SecretString::from("rotated"),
                refresh_token: Some(SecretString::from("next-refresh")),
                expires_at: Some(now()? + 3600),
                token_type: TokenType::Oauth,
            })
        }
    };
    let (a, b) = tokio::try_join!(
        stored_credential(vault.clone(), &store, "same-profile", false, renew),
        stored_credential(vault.clone(), &store, "same-profile", false, renew)
    )?;
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    assert_eq!(a.token.expose_secret(), b.token.expose_secret());
    assert_eq!(vault.read()?.token.expose_secret(), "rotated");
    Ok(())
}

#[tokio::test]
async fn failed_refresh_preserves_existing_credentials() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::for_test(directory.path().to_owned());
    let vault = MemoryVault(Arc::new(Mutex::new(Some(expired()))));
    let result = stored_credential(vault.clone(), &store, "profile", false, |_| async {
        bail!("simulated token server failure")
    })
    .await;
    result.err().context("operation unexpectedly succeeded")?;
    assert_eq!(
        vault
            .read()?
            .refresh_token
            .context("refresh token missing")?
            .expose_secret(),
        "refresh-old"
    );
    Ok(())
}

async fn request(address: std::net::SocketAddr, query: &str) -> Result<String> {
    let mut connection = tokio::net::TcpStream::connect(address).await?;
    connection
        .write_all(format!("GET /callback?{query} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
        .await?;
    let mut response = String::new();
    connection.read_to_string(&mut response).await?;
    Ok(response)
}

#[tokio::test]
async fn oauth_callback_rejects_wrong_state_then_accepts_the_matching_code() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let receiver =
        tokio::spawn(async move { callback(listener, "/callback", "expected-state").await });
    assert!(
        request(address, "state=wrong&code=attack")
            .await?
            .starts_with("HTTP/1.1 400")
    );
    assert!(
        request(address, "state=expected-state&code=accepted")
            .await?
            .starts_with("HTTP/1.1 200")
    );
    assert_eq!(receiver.await??, "accepted");
    Ok(())
}

#[tokio::test]
async fn oauth_denial_is_reported_without_waiting_for_timeout() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let receiver =
        tokio::spawn(async move { callback(listener, "/callback", "expected-state").await });
    request(address, "state=expected-state&error=access_denied").await?;
    receiver
        .await?
        .err()
        .context("operation unexpectedly succeeded")?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_refresh_keeps_lock_until_persistence_finishes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::for_test(directory.path().to_owned());
    let started = Arc::new(Notify::new());
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let unblock = ReleaseWriter(Arc::clone(&release));
    let memory = MemoryVault(Arc::new(Mutex::new(Some(expired()))));
    let vault = BlockingVault {
        memory: memory.clone(),
        started: Arc::clone(&started),
        release,
    };
    let refresh_store = store.clone();
    let job = tokio::spawn(async move {
        stored_credential(vault, &refresh_store, "writer", false, |_| async {
            Ok(Credential {
                token: SecretString::from("persisted"),
                refresh_token: Some(SecretString::from("next")),
                expires_at: Some(now()? + 3600),
                token_type: TokenType::Oauth,
            })
        })
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), started.notified()).await?;
    job.abort();
    let join_error = job.await.err().context("refresh was not cancelled")?;
    assert!(join_error.is_cancelled());
    let competing = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.path().join("oauth-writer.lock"))?;
    assert!(
        matches!(competing.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "cancellation released the OAuth lock while the writer was still active"
    );
    drop(unblock);
    let _lock =
        tokio::time::timeout(Duration::from_secs(2), store.lock_profile("writer")).await??;
    assert_eq!(memory.read()?.token.expose_secret(), "persisted");
    Ok(())
}

#[derive(Clone)]
struct BlockingVault {
    memory: MemoryVault,
    started: Arc<Notify>,
    release: Arc<(Mutex<bool>, Condvar)>,
}
impl Vault for BlockingVault {
    fn delete(&self) -> Result<()> {
        self.wait()?;
        self.memory.delete()
    }
    fn read(&self) -> Result<Credential> {
        self.memory.read()
    }
    fn save(&self, credential: &Credential) -> Result<()> {
        self.wait()?;
        self.memory.save(credential)
    }
}
struct ReleaseWriter(Arc<(Mutex<bool>, Condvar)>);
impl Drop for ReleaseWriter {
    fn drop(&mut self) {
        if let Ok(mut ready) = self.0.0.lock() {
            *ready = true;
            self.0.1.notify_one();
        }
    }
}

impl BlockingVault {
    fn wait(&self) -> Result<()> {
        self.started.notify_one();
        let (mutex, changed) = &*self.release;
        let ready = mutex.lock().map_err(|error| anyhow!("{error}"))?;
        let (ready, timeout) = changed
            .wait_timeout_while(ready, Duration::from_secs(5), |ready| !*ready)
            .map_err(|error| anyhow!("{error}"))?;
        if timeout.timed_out() && !*ready {
            bail!("test did not release the vault writer");
        }
        Ok(())
    }
}

fn metadata() -> config::Profile {
    config::Profile {
        token_type: TokenType::Oauth,
        user: Some("user".into()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_and_logout_wait_for_refresh_without_resurrecting_old_tokens() -> Result<()> {
    for logout in [false, true] {
        let directory = tempfile::tempdir()?;
        let store = Store::for_test(directory.path().to_owned());
        let vault = MemoryVault(Arc::new(Mutex::new(Some(expired()))));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let refresh_store = store.clone();
        let refresh_vault = vault.clone();
        let started = Arc::clone(&entered);
        let resume = Arc::clone(&release);
        let refresh = tokio::spawn(async move {
            stored_credential(refresh_vault, &refresh_store, "profile", false, move |_| {
                let started = Arc::clone(&started);
                let resume = Arc::clone(&resume);
                async move {
                    started.notify_one();
                    resume.notified().await;
                    let mut fresh = expired();
                    fresh.token = SecretString::from("refreshed");
                    fresh.expires_at = Some(now()? + 3600);
                    Ok(fresh)
                }
            })
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), entered.notified()).await?;
        let mutation_store = store.clone();
        let mutation_vault = vault.clone();
        let mut mutation = tokio::spawn(async move {
            if logout {
                delete_profile(mutation_vault, &mutation_store, "profile").await
            } else {
                let mut fresh = expired();
                fresh.token = SecretString::from("new-login");
                save_profile(
                    mutation_vault,
                    &mutation_store,
                    "profile",
                    fresh,
                    metadata(),
                )
                .await
            }
        });
        tokio::time::timeout(Duration::from_millis(80), &mut mutation)
            .await
            .err()
            .context("mutation bypassed the refresh lock")?;
        release.notify_one();
        refresh.await??;
        mutation.await??;
        let config = store.load().await?;
        if logout {
            vault
                .read()
                .err()
                .context("logout credential was resurrected")?;
            assert!(!config.profiles.contains_key("profile"));
            assert_eq!(config.active_profile, None);
        } else {
            assert_eq!(vault.read()?.token.expose_secret(), "new-login");
            assert_eq!(config.active_profile.as_deref(), Some("profile"));
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_profile_mutations_hold_both_locks_until_metadata_is_saved() -> Result<()> {
    for logout in [false, true] {
        let directory = tempfile::tempdir()?;
        let store = Store::for_test(directory.path().to_owned());
        let memory = MemoryVault(Arc::new(Mutex::new(Some(expired()))));
        save_profile(memory.clone(), &store, "writer", expired(), metadata()).await?;
        let started = Arc::new(Notify::new());
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let unblock = ReleaseWriter(Arc::clone(&release));
        let vault = BlockingVault {
            memory: memory.clone(),
            started: Arc::clone(&started),
            release,
        };
        let writer_store = store.clone();
        let job = tokio::spawn(async move {
            if logout {
                delete_profile(vault, &writer_store, "writer").await
            } else {
                let mut credential = expired();
                credential.token = SecretString::from("saved-login");
                save_profile(vault, &writer_store, "writer", credential, metadata()).await
            }
        });
        tokio::time::timeout(Duration::from_secs(2), started.notified()).await?;
        job.abort();
        assert!(
            job.await
                .err()
                .context("writer was not cancelled")?
                .is_cancelled()
        );
        for name in ["oauth-writer.lock", "config.lock"] {
            let competing = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(directory.path().join(name))?;
            assert!(
                matches!(competing.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
                "released {name} during persistence"
            );
        }
        drop(unblock);
        let _lock =
            tokio::time::timeout(Duration::from_secs(2), store.lock_profile("writer")).await??;
        let config = store.load().await?;
        if logout {
            memory.read().err().context("credential was not deleted")?;
            assert!(!config.profiles.contains_key("writer"));
            assert_eq!(config.active_profile, None);
        } else {
            assert_eq!(memory.read()?.token.expose_secret(), "saved-login");
            assert_eq!(config.active_profile.as_deref(), Some("writer"));
        }
    }
    Ok(())
}
