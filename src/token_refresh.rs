//! Cross-process-safe access token refresh.
//!
//! The paper tick and interactive sessions both run rhap, and each may find
//! the stored access token expired at the same moment. [`current_tokens`]
//! serialises refresh with an exclusive `flock` and re-reads the tokens under
//! it, so only one process spends the refresh token.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use pk_cli_config::ConfigStore;
use pk_cli_core::CliError;
use pk_cli_secrets::CredentialStore;

use crate::config::BIN;
use crate::credential::{self, TokenSet};

/// Where the stored token set is read from and written to. The keychain in
/// production; an in-memory fake in tests, which never touch the keychain.
pub(crate) trait TokenStore {
    fn read(&mut self) -> Result<Option<TokenSet>, CliError>;
    fn write(&mut self, tokens: &TokenSet) -> Result<(), CliError>;
}

pub(crate) struct KeychainTokens<'a>(pub(crate) &'a CredentialStore);

impl TokenStore for KeychainTokens<'_> {
    fn read(&mut self) -> Result<Option<TokenSet>, CliError> {
        credential::read_tokens(self.0)
    }
    fn write(&mut self, tokens: &TokenSet) -> Result<(), CliError> {
        credential::write_tokens(self.0, tokens)
    }
}

/// The refresh lock: `token-refresh.lock` beside the default `config.json`
/// (`${XDG_CONFIG_HOME:-~/.config}/rhap/`). Deliberately not the `--config`
/// override's directory: the keychain entry is machine-wide, so every rhap
/// process must contend on the same file whatever config it was given.
pub(crate) fn refresh_lock_path() -> Result<PathBuf, CliError> {
    let config = ConfigStore::new(BIN).path()?;
    let dir = config
        .parent()
        .ok_or_else(|| CliError::Other(format!("{} has no parent", config.display())))?;
    Ok(dir.join("token-refresh.lock"))
}

/// An exclusive `flock` on the lock file, released when dropped (and by the
/// kernel if the process dies holding it).
struct RefreshLock(File);

impl RefreshLock {
    fn acquire(path: &Path) -> Result<Self, CliError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| CliError::Other(format!("creating {}: {e}", dir.display())))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(|e| CliError::Other(format!("opening {}: {e}", path.display())))?;
        file.lock()
            .map_err(|e| CliError::Other(format!("locking {}: {e}", path.display())))?;
        Ok(Self(file))
    }
}

impl Drop for RefreshLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// The stored tokens, refreshed first if the access token has expired.
///
/// Refresh runs under an exclusive lock, and the tokens are re-read once the
/// lock is held. Without it, two processes (the paper tick and a session both
/// call rhap) could both see an expired token and both refresh; with a
/// rotating refresh token the second refresh spends a token the first just
/// replaced, and the loser's write can leave a dead refresh token in the
/// keychain (audit 2026-09-23, security #8). Under the lock the second process
/// finds the first one's fresh token and does not refresh at all.
pub(crate) fn current_tokens<S, N, R>(
    store: &mut S,
    lock_path: &Path,
    now: N,
    refresh: R,
) -> Result<TokenSet, CliError>
where
    S: TokenStore,
    N: Fn() -> u64,
    R: FnOnce(&TokenSet) -> Result<TokenSet, CliError>,
{
    let not_logged_in = || CliError::Auth("not logged in — run `rhap auth login`".into());
    let tokens = store.read()?.ok_or_else(not_logged_in)?;
    if !tokens.access_expired(now()) {
        return Ok(tokens);
    }

    let _lock = RefreshLock::acquire(lock_path)?;
    // Another process may have refreshed while this one waited for the lock.
    let tokens = store.read()?.ok_or_else(not_logged_in)?;
    if !tokens.access_expired(now()) {
        return Ok(tokens);
    }
    let fresh = refresh(&tokens)?;
    store.write(&fresh)?;
    Ok(fresh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier, Mutex};

    fn tokens(access: &str, expires_at: u64) -> TokenSet {
        TokenSet {
            access_token: access.into(),
            refresh_token: format!("refresh-{access}"),
            expires_at,
            scope: None,
            token_type: None,
        }
    }

    fn scratch_lock(tag: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("rhap-lock-test-{}-{tag}", std::process::id()))
            .join("token-refresh.lock")
    }

    /// Returns each queued read in turn (the last one repeats), and records
    /// writes.
    struct Scripted {
        reads: Vec<Option<TokenSet>>,
        served: usize,
        written: Vec<TokenSet>,
    }

    impl TokenStore for Scripted {
        fn read(&mut self) -> Result<Option<TokenSet>, CliError> {
            let i = self.served.min(self.reads.len() - 1);
            self.served += 1;
            Ok(self.reads[i].clone())
        }
        fn write(&mut self, t: &TokenSet) -> Result<(), CliError> {
            self.written.push(t.clone());
            Ok(())
        }
    }

    const NOW: u64 = 10_000;

    #[test]
    fn a_valid_token_is_used_without_the_lock_or_a_refresh() {
        let mut s = Scripted {
            reads: vec![Some(tokens("live", NOW + 3_600))],
            served: 0,
            written: vec![],
        };
        let lock = scratch_lock("valid");
        let t = current_tokens(&mut s, &lock, || NOW, |_| panic!("no refresh")).unwrap();
        assert_eq!(t.access_token, "live");
        assert_eq!(s.served, 1);
        assert!(!lock.exists(), "a valid token never touches the lock");
    }

    /// The seam for the cross-process case: expired on the first read, fresh
    /// on the re-read under the lock (another process refreshed meanwhile).
    #[test]
    fn a_token_refreshed_by_another_process_is_reread_not_refreshed() {
        let mut s = Scripted {
            reads: vec![
                Some(tokens("stale", NOW - 1)),
                Some(tokens("theirs", NOW + 3_600)),
            ],
            served: 0,
            written: vec![],
        };
        let t = current_tokens(
            &mut s,
            &scratch_lock("reread"),
            || NOW,
            |_| panic!("the re-read token is fresh; refreshing would spend a rotated token"),
        )
        .unwrap();
        assert_eq!(t.access_token, "theirs");
        assert_eq!(s.served, 2, "read, then re-read under the lock");
        assert!(s.written.is_empty());
    }

    #[test]
    fn a_still_expired_token_is_refreshed_once_and_stored() {
        let mut s = Scripted {
            reads: vec![Some(tokens("stale", NOW - 1))],
            served: 0,
            written: vec![],
        };
        let t = current_tokens(
            &mut s,
            &scratch_lock("refresh"),
            || NOW,
            |stale| {
                assert_eq!(stale.refresh_token, "refresh-stale");
                Ok(tokens("new", NOW + 3_600))
            },
        )
        .unwrap();
        assert_eq!(t.access_token, "new");
        assert_eq!(s.served, 2);
        assert_eq!(s.written.len(), 1);
        assert_eq!(s.written[0].access_token, "new");
    }

    #[test]
    fn logged_out_is_an_auth_error_on_either_read() {
        let mut s = Scripted {
            reads: vec![None],
            served: 0,
            written: vec![],
        };
        let e = current_tokens(&mut s, &scratch_lock("out1"), || NOW, |_| unreachable!());
        assert_eq!(e.unwrap_err().exit_code(), 3);
        let mut s = Scripted {
            reads: vec![Some(tokens("stale", NOW - 1)), None],
            served: 0,
            written: vec![],
        };
        let e = current_tokens(&mut s, &scratch_lock("out2"), || NOW, |_| unreachable!());
        assert_eq!(e.unwrap_err().exit_code(), 3);
    }

    /// A shared store behind a mutex stands in for the keychain.
    struct Shared(Arc<Mutex<TokenSet>>);

    impl TokenStore for Shared {
        fn read(&mut self) -> Result<Option<TokenSet>, CliError> {
            Ok(Some(self.0.lock().unwrap().clone()))
        }
        fn write(&mut self, t: &TokenSet) -> Result<(), CliError> {
            *self.0.lock().unwrap() = t.clone();
            Ok(())
        }
    }

    /// Two contenders see the same expired token; the flock serialises them
    /// (separate open file descriptions contend even within one process), so
    /// exactly one refreshes and the other uses its result.
    #[test]
    fn concurrent_refreshes_are_serialised_by_the_lock() {
        let keychain = Arc::new(Mutex::new(tokens("stale", NOW - 1)));
        let refreshes = Arc::new(Mutex::new(0u32));
        let lock = scratch_lock("race");
        let start = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let (keychain, refreshes, lock, start) = (
                    Arc::clone(&keychain),
                    Arc::clone(&refreshes),
                    lock.clone(),
                    Arc::clone(&start),
                );
                std::thread::spawn(move || {
                    let mut store = Shared(keychain);
                    start.wait();
                    current_tokens(
                        &mut store,
                        &lock,
                        || NOW,
                        |_| {
                            *refreshes.lock().unwrap() += 1;
                            std::thread::sleep(std::time::Duration::from_millis(200));
                            Ok(tokens("new", NOW + 3_600))
                        },
                    )
                    .unwrap()
                    .access_token
                })
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), "new");
        }
        assert_eq!(*refreshes.lock().unwrap(), 1, "exactly one refresh");
    }
}
