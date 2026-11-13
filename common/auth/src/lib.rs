//! Admin token authentication for all `/api/*` endpoints, plus the admin
//! password guard that protects every configuration write (F-F01..F-F05).
//!
//! Error codes map to docs/ERROR-CODES.md and contracts/openapi.yaml.
//! Password digests are PBKDF2-HMAC-SHA256 with a per-install random salt,
//! stored in a `0600` file; verification is constant time.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use common_crypto::{constant_eq, from_hex, pbkdf2_sha256, random_hex, to_hex};

pub const ALGORITHM: &str = "pbkdf2_sha256";
pub const ITERATIONS: u32 = 200_000;
pub const SALT_BYTES: usize = 16;
pub const HASH_BYTES: usize = 32;
pub const MIN_LENGTH: usize = 8;
pub const MAX_LENGTH: usize = 256;

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    Required,
    Failed,
}

pub fn check(
    headers: &HashMap<String, String>,
    query: &str,
    expected: &str,
    loopback: bool,
) -> Result<(), AuthError> {
    if expected.is_empty() {
        return if loopback {
            Ok(())
        } else {
            Err(AuthError::Required)
        };
    }
    let mut supplied = headers.get("x-admin-token").cloned().unwrap_or_default();
    if supplied.is_empty() {
        supplied = query_token(query);
    }
    if supplied.is_empty() {
        return Err(AuthError::Required);
    }
    if constant_eq(supplied.as_bytes(), expected.as_bytes()) {
        Ok(())
    } else {
        Err(AuthError::Failed)
    }
}

fn query_token(query: &str) -> String {
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            if key == "token" {
                return value.to_string();
            }
        }
    }
    String::new()
}

// ---------------------------------------------------------------- password

/// Read-only view used by `common-api` to protect write routes. Implemented
/// here so the HTTP layer never needs to know the digest format.
pub trait PasswordGuard: Send + Sync {
    fn is_set(&self) -> bool;
    fn verify(&self, password: &str) -> bool;
}

#[derive(Debug, PartialEq, Eq)]
pub enum WriteGuardError {
    /// No password has been configured yet: writes must go through the
    /// first-time setup flow (HTTP 409, `setup_required`).
    SetupRequired,
    /// Missing or wrong `X-Admin-Password` (HTTP 403, `auth_failed`).
    AuthFailed,
}

/// Enforce the admin password on configuration writes. See ERROR-CODES.md §3.
pub fn check_write(
    headers: &HashMap<String, String>,
    guard: Option<&dyn PasswordGuard>,
) -> Result<(), WriteGuardError> {
    let guard = guard.ok_or(WriteGuardError::SetupRequired)?;
    if !guard.is_set() {
        return Err(WriteGuardError::SetupRequired);
    }
    let supplied = headers.get("x-admin-password").cloned().unwrap_or_default();
    if supplied.is_empty() || !guard.verify(&supplied) {
        return Err(WriteGuardError::AuthFailed);
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum PasswordError {
    TooShort,
    TooLong,
    AlreadySet,
    NotSet,
    Mismatch,
    Io(String),
}

impl std::fmt::Display for PasswordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::TooShort => "password too short",
            Self::TooLong => "password too long",
            Self::AlreadySet => "password already set; use change()",
            Self::NotSet => "password not set; use set()",
            Self::Mismatch => "current password does not match",
            Self::Io(message) => message,
        };
        f.write_str(text)
    }
}

#[derive(Clone)]
struct Stored {
    iterations: u32,
    salt: Vec<u8>,
    hash: Vec<u8>,
}

impl Stored {
    fn encode(&self) -> String {
        format!(
            "{}${}${}${}\n",
            ALGORITHM,
            self.iterations,
            to_hex(&self.salt),
            to_hex(&self.hash)
        )
    }

    fn decode(text: &str) -> Result<Self, PasswordError> {
        let parts: Vec<&str> = text.trim().split('$').collect();
        if parts.len() != 4 || parts[0] != ALGORITHM {
            return Err(PasswordError::Io(
                "unsupported password digest format".into(),
            ));
        }
        let iterations: u32 = parts[1]
            .parse()
            .map_err(|_| PasswordError::Io("invalid iteration count".into()))?;
        if iterations == 0 {
            return Err(PasswordError::Io("invalid iteration count".into()));
        }
        let salt =
            from_hex(parts[2]).ok_or_else(|| PasswordError::Io("invalid salt hex".into()))?;
        let hash =
            from_hex(parts[3]).ok_or_else(|| PasswordError::Io("invalid hash hex".into()))?;
        if salt.is_empty() || hash.len() != HASH_BYTES {
            return Err(PasswordError::Io("invalid digest length".into()));
        }
        Ok(Self {
            iterations,
            salt,
            hash,
        })
    }

    fn matches(&self, password: &str) -> bool {
        let derived = pbkdf2_sha256(password.as_bytes(), &self.salt, self.iterations, HASH_BYTES);
        constant_eq(&derived, &self.hash)
    }
}

pub struct PasswordManager {
    path: PathBuf,
    iterations: u32,
    stored: RwLock<Option<Stored>>,
}

impl PasswordManager {
    /// Load from `path` using the production iteration count.
    pub fn load(path: impl Into<PathBuf>) -> io::Result<Arc<Self>> {
        Self::load_with_iterations(path, ITERATIONS)
    }

    /// Load with a custom PBKDF2 iteration count. Production uses `load()`
    /// (200_000); tests use a small value so debug builds stay fast. The
    /// iteration count of an existing digest file always wins at verify time.
    pub fn load_with_iterations(
        path: impl Into<PathBuf>,
        iterations: u32,
    ) -> io::Result<Arc<Self>> {
        let path = path.into();
        let stored = match fs::read_to_string(&path) {
            Ok(text) => Some(
                Stored::decode(&text)
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?,
            ),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => return Err(err),
        };
        Ok(Arc::new(Self {
            path,
            iterations: iterations.max(1),
            stored: RwLock::new(stored),
        }))
    }

    /// First-time setup. Requires that no password exists yet.
    pub fn set(&self, password: &str) -> Result<(), PasswordError> {
        validate_password(password)?;
        if self.is_set() {
            return Err(PasswordError::AlreadySet);
        }
        let stored = self.derive(password);
        self.persist(&stored)?;
        *self.stored.write().unwrap() = Some(stored);
        Ok(())
    }

    /// Change an existing password, proving knowledge of the current one.
    pub fn change(&self, current: &str, new: &str) -> Result<(), PasswordError> {
        validate_password(new)?;
        {
            let guard = self.stored.read().unwrap();
            let existing = guard.as_ref().ok_or(PasswordError::NotSet)?;
            if !existing.matches(current) {
                return Err(PasswordError::Mismatch);
            }
        }
        let stored = self.derive(new);
        self.persist(&stored)?;
        *self.stored.write().unwrap() = Some(stored);
        Ok(())
    }

    fn derive(&self, password: &str) -> Stored {
        let salt = from_hex(&random_hex(SALT_BYTES)).unwrap_or_else(|| vec![0u8; SALT_BYTES]);
        let hash = pbkdf2_sha256(password.as_bytes(), &salt, self.iterations, HASH_BYTES);
        Stored {
            iterations: self.iterations,
            salt,
            hash,
        }
    }

    fn persist(&self, stored: &Stored) -> Result<(), PasswordError> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|err| PasswordError::Io(err.to_string()))?;
            }
        }
        // Write to a sibling temp file, then atomically rename. The digest
        // file always ends up mode 0600 (F-F03).
        let temp = self.path.with_extension("tmp");
        {
            let mut options = fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&temp)
                .map_err(|err| PasswordError::Io(err.to_string()))?;
            file.write_all(stored.encode().as_bytes())
                .map_err(|err| PasswordError::Io(err.to_string()))?;
            file.sync_all()
                .map_err(|err| PasswordError::Io(err.to_string()))?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))
                .map_err(|err| PasswordError::Io(err.to_string()))?;
        }
        fs::rename(&temp, &self.path).map_err(|err| PasswordError::Io(err.to_string()))?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl PasswordGuard for PasswordManager {
    fn is_set(&self) -> bool {
        self.stored.read().unwrap().is_some()
    }

    fn verify(&self, password: &str) -> bool {
        self.stored
            .read()
            .unwrap()
            .as_ref()
            .is_some_and(|stored| stored.matches(password))
    }
}

pub fn validate_password(password: &str) -> Result<(), PasswordError> {
    if password.chars().count() < MIN_LENGTH {
        return Err(PasswordError::TooShort);
    }
    if password.chars().count() > MAX_LENGTH {
        return Err(PasswordError::TooLong);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(token: &str) -> HashMap<String, String> {
        let mut map = HashMap::new();
        map.insert("x-admin-token".to_string(), token.to_string());
        map
    }

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "lanstudio-auth-{}-{}-{}.txt",
            label,
            std::process::id(),
            random_hex(4)
        ))
    }

    #[test]
    fn empty_token_allows_loopback_only() {
        assert_eq!(check(&HashMap::new(), "", "", true), Ok(()));
        assert_eq!(
            check(&HashMap::new(), "", "", false),
            Err(AuthError::Required)
        );
    }

    #[test]
    fn token_from_header_or_query() {
        assert_eq!(check(&headers("s3cret"), "", "s3cret", false), Ok(()));
        assert_eq!(
            check(&HashMap::new(), "token=s3cret", "s3cret", false),
            Ok(())
        );
        assert_eq!(
            check(&headers("wrong"), "", "s3cret", false),
            Err(AuthError::Failed)
        );
    }

    #[test]
    fn write_guard_requires_setup_then_password() {
        let path = temp_path("guard");
        let manager = PasswordManager::load_with_iterations(&path, 600).unwrap();
        let guard: &dyn PasswordGuard = manager.as_ref();
        assert_eq!(
            check_write(&HashMap::new(), Some(guard)),
            Err(WriteGuardError::SetupRequired)
        );
        manager.set("correct horse").unwrap();
        assert_eq!(
            check_write(&HashMap::new(), Some(guard)),
            Err(WriteGuardError::AuthFailed)
        );
        let mut bad = HashMap::new();
        bad.insert("x-admin-password".into(), "wrong".into());
        assert_eq!(
            check_write(&bad, Some(guard)),
            Err(WriteGuardError::AuthFailed)
        );
        let mut good = HashMap::new();
        good.insert("x-admin-password".into(), "correct horse".into());
        assert_eq!(check_write(&good, Some(guard)), Ok(()));
        assert_eq!(
            check_write(&good, None),
            Err(WriteGuardError::SetupRequired)
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn password_digest_roundtrip_and_change() {
        let path = temp_path("roundtrip");
        let manager = PasswordManager::load_with_iterations(&path, 600).unwrap();
        assert!(!manager.is_set());
        assert!(matches!(manager.set("short"), Err(PasswordError::TooShort)));
        manager.set("first-password").unwrap();
        assert!(manager.verify("first-password"));
        assert!(!manager.verify("first-passworD"));

        let reloaded = PasswordManager::load_with_iterations(&path, 600).unwrap();
        assert!(reloaded.is_set());
        assert!(reloaded.verify("first-password"));

        assert!(matches!(
            manager.change("wrong-current", "second-password"),
            Err(PasswordError::Mismatch)
        ));
        manager.change("first-password", "second-password").unwrap();
        assert!(manager.verify("second-password"));
        assert!(!manager.verify("first-password"));
        assert!(matches!(
            manager.set("third-password"),
            Err(PasswordError::AlreadySet)
        ));

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("pbkdf2_sha256$600$"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "digest file must be 0600");
        }
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn malformed_digest_file_is_a_hard_error() {
        let path = temp_path("malformed");
        fs::write(&path, "not-a-digest\n").unwrap();
        assert!(PasswordManager::load_with_iterations(&path, 600).is_err());
        let _ = fs::remove_file(&path);
    }
}
