//! Where connection passwords live.
//!
//! - The OS credential store when there is one: Windows Credential Manager, or the Secret Service
//!   (GNOME Keyring / KWallet) on Linux desktops.
//! - Otherwise (e.g. WSL, headless Linux) an encrypted vault file unlocked with a master
//!   password: Argon2id derives the key, ChaCha20-Poly1305 encrypts each password.
//!
//! Passwords are never written to disk in plain text.

use std::collections::BTreeMap;
use std::path::PathBuf;

use argon2::Argon2;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, Generate, Key, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use eframe::egui;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::store::{self, Loaded, StoreError};

const SERVICE: &str = "tabella";
/// The app's name before it became tabella: passwords stored under it move over when read.
const OLD_SERVICE: &str = "dblite";
/// Encrypted with the vault key to tell a wrong master password from a corrupted file. Still the
/// old name: changing it would make every existing vault refuse its master password.
const CHECK: &[u8] = b"dblite-vault-ok";

/// A password in memory: wiped when dropped, and `Debug` never shows it. Reads as a `String`.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::ops::Deref for Secret {
    type Target = String;

    fn deref(&self) -> &String {
        &self.0
    }
}

impl Secret {
    /// Empties it, wiping what it held (`String::clear` leaves the bytes in the buffer).
    pub fn clear(&mut self) {
        self.0.zeroize();
    }
}

/// Typed into a `TextEdit` without `DerefMut`: a `String` that grows moves to a bigger buffer
/// and frees the old one as it is, so the password would stay behind in freed memory.
impl egui::TextBuffer for Secret {
    fn is_mutable(&self) -> bool {
        true
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    fn insert_text(&mut self, text: &str, char_index: egui::text::CharIndex) -> usize {
        let needed = self.0.len() + text.len();
        if needed > self.0.capacity() {
            let mut bigger = String::with_capacity(needed.max(2 * self.0.capacity()).max(32));
            bigger.push_str(&self.0);
            // The old buffer is wiped when the old `Secret` drops.
            drop(std::mem::replace(self, Secret(bigger)));
        }
        self.0.insert_text(text, char_index)
    }

    fn delete_char_range(&mut self, char_range: std::ops::Range<egui::text::CharIndex>) {
        // Shrinks in place: the bytes left past the end are wiped with the rest on drop.
        self.0.delete_char_range(char_range);
    }

    fn type_id(&self) -> std::any::TypeId {
        std::any::TypeId::of::<Self>()
    }
}

impl From<String> for Secret {
    fn from(s: String) -> Self {
        Secret(s)
    }
}

impl From<&str> for Secret {
    fn from(s: &str) -> Self {
        Secret(s.to_string())
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_empty() { "\"\"" } else { "\"***\"" })
    }
}

pub enum SecretStore {
    Os,
    Vault(Vault),
}

impl SecretStore {
    pub fn detect() -> Self {
        if os::available() {
            SecretStore::Os
        } else {
            SecretStore::Vault(Vault::load(vault_path()))
        }
    }

    #[cfg(test)]
    pub fn vault_at(path: PathBuf) -> Self {
        SecretStore::Vault(Vault::load(Some(path)))
    }

    /// Short label for the UI.
    pub fn describe(&self) -> &'static str {
        match self {
            SecretStore::Os if cfg!(windows) => "Windows Credential Manager",
            SecretStore::Os => "system keyring",
            SecretStore::Vault(_) => "master-password vault",
        }
    }

    pub fn get(&self, name: &str) -> Option<Secret> {
        match self {
            SecretStore::Os => os::get(name).map(Secret),
            SecretStore::Vault(v) => v.get(name).map(Secret),
        }
    }

    pub fn set(&mut self, name: &str, password: &str) -> Result<(), SecretError> {
        match self {
            SecretStore::Os => os::set(name, password)
                .then_some(())
                .ok_or(SecretError::Unavailable),
            SecretStore::Vault(v) => v.set(name, password),
        }
    }

    /// Removing a name that isn't stored is not an error.
    pub fn delete(&mut self, name: &str) -> Result<(), SecretError> {
        match self {
            SecretStore::Os => os::delete(name).then_some(()).ok_or(SecretError::Unavailable),
            SecretStore::Vault(v) => v.delete(name),
        }
    }

    /// Why the vault file could not be read at startup, once.
    pub fn take_warning(&mut self) -> Option<String> {
        match self {
            SecretStore::Vault(v) => v.warning.take(),
            SecretStore::Os => None,
        }
    }

    /// The vault, when passwords go there and it still needs the master password.
    pub fn locked_vault(&self) -> Option<&Vault> {
        match self {
            SecretStore::Vault(v) if !v.is_unlocked() => Some(v),
            _ => None,
        }
    }

    pub fn vault_mut(&mut self) -> Option<&mut Vault> {
        match self {
            SecretStore::Vault(v) => Some(v),
            SecretStore::Os => None,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum SecretError {
    /// The vault exists (or must be created) and needs the master password first.
    Locked,
    Unavailable,
}

/// Why the vault couldn't be created or unlocked. Shown to the user as is.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("Use at least 8 characters")]
    TooShort,
    #[error("Wrong master password")]
    WrongPassword,
    #[error("the vault hasn't been created yet")]
    NotCreated,
    #[error("corrupted vault")]
    Corrupted,
    #[error("failed to derive the key")]
    Kdf,
    #[error("encryption failed")]
    Encryption,
    #[error("couldn't save the vault: {0}")]
    Save(#[from] StoreError),
}

#[derive(Serialize, Deserialize, Default)]
struct VaultFile {
    salt: String,
    check: Sealed,
    entries: BTreeMap<String, Sealed>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct Sealed {
    nonce: String,
    data: String,
}

/// Input for the key derivation, sendable to another thread. Secrets here and below are wiped
/// from memory when dropped (`Zeroizing`, and the cipher's own key with the `zeroize` feature).
pub struct KdfJob {
    master: Secret,
    salt: Vec<u8>,
    create: bool,
}

impl KdfJob {
    /// Argon2id: deliberately slow (to resist guessing), so never on the UI thread.
    pub fn run(self) -> Result<Derived, VaultError> {
        let cipher = Vault::derive(&self.master, &self.salt)?;
        Ok(Derived {
            cipher,
            salt: self.salt,
            create: self.create,
        })
    }
}

pub struct Derived {
    cipher: ChaCha20Poly1305,
    salt: Vec<u8>,
    create: bool,
}

pub struct Vault {
    path: Option<PathBuf>,
    file: Option<VaultFile>,
    cipher: Option<ChaCha20Poly1305>,
    warning: Option<String>,
}

fn vault_path() -> Option<PathBuf> {
    store::path("vault.json").ok()
}

impl Vault {
    fn load(path: Option<PathBuf>) -> Self {
        let Loaded { value: file, warning } = match &path {
            Some(p) => store::load_at(p),
            None => Loaded {
                value: None,
                warning: None,
            },
        };
        Vault {
            path,
            file,
            cipher: None,
            warning,
        }
    }

    /// Whether a master password was already set up.
    pub fn exists(&self) -> bool {
        self.file.is_some()
    }

    pub fn is_unlocked(&self) -> bool {
        self.cipher.is_some()
    }

    /// Whether the vault holds a password for this connection (readable even while locked).
    pub fn has(&self, name: &str) -> bool {
        self.file.as_ref().is_some_and(|f| f.entries.contains_key(name))
    }

    fn derive(master: &str, salt: &[u8]) -> Result<ChaCha20Poly1305, VaultError> {
        let mut key = Zeroizing::new([0u8; 32]);
        Argon2::default()
            .hash_password_into(master.as_bytes(), salt, &mut *key)
            .map_err(|_| VaultError::Kdf)?;
        ChaCha20Poly1305::new_from_slice(&*key).map_err(|_| VaultError::Kdf)
    }

    /// First half of create/unlock, cheap: checks the input and picks the salt. The slow key
    /// derivation runs in `KdfJob::run`, off the UI thread; `finish` applies its result.
    pub fn kdf_job(&self, master: &str, create: bool) -> Result<KdfJob, VaultError> {
        let salt = if create {
            if master.chars().count() < 8 {
                return Err(VaultError::TooShort);
            }
            // A random 32-byte value; the key type is just a convenient generator.
            Key::<ChaCha20Poly1305>::generate().to_vec()
        } else {
            let file = self.file.as_ref().ok_or(VaultError::NotCreated)?;
            B64.decode(&file.salt).map_err(|_| VaultError::Corrupted)?
        };
        Ok(KdfJob {
            master: master.into(),
            salt,
            create,
        })
    }

    pub fn finish(&mut self, derived: Derived) -> Result<(), VaultError> {
        let Derived { cipher, salt, create } = derived;
        if create {
            let check = seal(&cipher, CHECK)?;
            self.file = Some(VaultFile {
                salt: B64.encode(salt),
                check,
                entries: BTreeMap::new(),
            });
            self.cipher = Some(cipher);
            return Ok(self.save()?);
        }
        let file = self.file.as_ref().ok_or(VaultError::NotCreated)?;
        match open(&cipher, &file.check) {
            Some(v) if *v == CHECK => {
                self.cipher = Some(cipher);
                Ok(())
            }
            _ => Err(VaultError::WrongPassword),
        }
    }

    /// Sets up a new vault with this master password (blocking; the UI uses `kdf_job`).
    #[cfg(test)]
    pub fn create(&mut self, master: &str) -> Result<(), VaultError> {
        let derived = self.kdf_job(master, true)?.run()?;
        self.finish(derived)
    }

    #[cfg(test)]
    pub fn unlock(&mut self, master: &str) -> Result<(), VaultError> {
        let derived = self.kdf_job(master, false)?.run()?;
        self.finish(derived)
    }

    fn get(&self, name: &str) -> Option<String> {
        let cipher = self.cipher.as_ref()?;
        let sealed = self.file.as_ref()?.entries.get(name)?;
        // Takes the buffer over: the plain text isn't copied, and leaves with the caller.
        String::from_utf8(std::mem::take(&mut *open(cipher, sealed)?)).ok()
    }

    fn set(&mut self, name: &str, password: &str) -> Result<(), SecretError> {
        let (Some(cipher), Some(file)) = (&self.cipher, &mut self.file) else {
            return Err(SecretError::Locked);
        };
        let sealed = seal(cipher, password.as_bytes()).map_err(|_| SecretError::Unavailable)?;
        file.entries.insert(name.to_string(), sealed);
        self.save().map_err(|_| SecretError::Unavailable)
    }

    fn delete(&mut self, name: &str) -> Result<(), SecretError> {
        let removed = self
            .file
            .as_mut()
            .is_some_and(|f| f.entries.remove(name).is_some());
        if removed {
            self.save().map_err(|_| SecretError::Unavailable)
        } else {
            Ok(())
        }
    }

    fn save(&self) -> Result<(), StoreError> {
        let (Some(path), Some(file)) = (&self.path, &self.file) else {
            return Ok(());
        };
        store::save_at(path, file)
    }
}

fn seal(cipher: &ChaCha20Poly1305, plain: &[u8]) -> Result<Sealed, VaultError> {
    let nonce = Nonce::generate();
    let data = cipher
        .encrypt(&nonce, plain)
        .map_err(|_| VaultError::Encryption)?;
    Ok(Sealed {
        nonce: B64.encode(nonce),
        data: B64.encode(data),
    })
}

/// `None` for a wrong key or tampered data (the AEAD tag doesn't match).
fn open(cipher: &ChaCha20Poly1305, sealed: &Sealed) -> Option<Zeroizing<Vec<u8>>> {
    let nonce = B64.decode(&sealed.nonce).ok()?;
    let nonce = Nonce::try_from(nonce.as_slice()).ok()?;
    cipher
        .decrypt(&nonce, B64.decode(&sealed.data).ok()?.as_ref())
        .ok()
        .map(Zeroizing::new)
}

#[cfg(any(windows, target_os = "linux"))]
mod os {
    use super::{OLD_SERVICE, SERVICE};

    fn entry(name: &str) -> Option<keyring::Entry> {
        keyring::Entry::new(SERVICE, name).ok()
    }

    fn old_entry(name: &str) -> Option<keyring::Entry> {
        keyring::Entry::new(OLD_SERVICE, name).ok()
    }

    /// On Linux the Secret Service may simply not be running (WSL, servers).
    pub fn available() -> bool {
        if cfg!(windows) {
            return true;
        }
        matches!(
            entry("__tabella_probe__").map(|e| e.get_password()),
            Some(Ok(_) | Err(keyring::Error::NoEntry))
        )
    }

    pub fn set(name: &str, password: &str) -> bool {
        entry(name).is_some_and(|e| e.set_password(password).is_ok())
    }

    pub fn get(name: &str) -> Option<String> {
        if let Some(password) = entry(name).and_then(|e| e.get_password().ok()) {
            return Some(password);
        }
        // Saved before the rename: copy it under the new name, then drop the old copy (only
        // once the new one is stored, so a failure never loses it).
        let old = old_entry(name)?;
        let password = old.get_password().ok()?;
        if set(name, &password) {
            let _ = old.delete_credential();
        }
        Some(password)
    }

    /// True when the credential is gone (or never existed).
    pub fn delete(name: &str) -> bool {
        let gone = |e: Option<keyring::Entry>| {
            e.is_some_and(|e| matches!(e.delete_credential(), Ok(()) | Err(keyring::Error::NoEntry)))
        };
        gone(entry(name)) & gone(old_entry(name))
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod os {
    pub fn available() -> bool {
        false
    }
    pub fn set(_: &str, _: &str) -> bool {
        false
    }
    pub fn get(_: &str) -> Option<String> {
        None
    }
    pub fn delete(_: &str) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_edits_like_a_string() {
        use egui::TextBuffer as _;
        use egui::text::CharIndex;
        let mut s = Secret::default();
        // Past the first buffer's capacity: it moves to a bigger one on the way.
        for (i, ch) in "correct horse battery staple, twice: correct horse"
            .chars()
            .enumerate()
        {
            s.insert_text(&ch.to_string(), CharIndex(i));
        }
        s.insert_text("é", CharIndex(0));
        s.delete_char_range(CharIndex(1)..CharIndex(9));
        assert_eq!(s.as_str(), "éhorse battery staple, twice: correct horse");
        s.clear();
        assert!(s.is_empty());
    }

    #[test]
    fn a_secret_is_saved_as_plain_text_but_never_debug_printed() {
        let s = Secret::from("hunter2");
        assert_eq!(format!("{s:?}"), "\"***\"");
        assert_eq!(format!("{:?}", Secret::default()), "\"\"");
        assert_eq!(serde_json::to_string(&s).unwrap(), "\"hunter2\"");
        assert_eq!(serde_json::from_str::<Secret>("\"hunter2\"").unwrap(), s);
    }

    fn temp_vault() -> (Vault, PathBuf) {
        let dir = std::env::temp_dir().join(format!("tabella-vault-test-{}", Nonce::generate()[0]));
        let path = dir.join("vault.json");
        let _ = std::fs::remove_file(&path);
        (Vault::load(Some(path.clone())), path)
    }

    #[test]
    fn roundtrip_and_reopen() {
        let (mut v, path) = temp_vault();
        assert!(!v.exists());
        assert_eq!(v.set("dev", "x"), Err(SecretError::Locked));
        assert!(v.create("short").is_err());
        v.create("good-master-password").unwrap();
        v.set("dev", "s3cr3t café").unwrap();
        assert_eq!(v.get("dev").as_deref(), Some("s3cr3t café"));

        // The file never contains the password.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("s3cr3t"));

        // Reopened from disk: locked until the right master password.
        let mut again = Vault::load(Some(path.clone()));
        assert!(again.exists() && !again.is_unlocked() && again.has("dev"));
        assert_eq!(again.get("dev"), None);
        assert!(matches!(
            again.unlock("wrong-wrong"),
            Err(VaultError::WrongPassword)
        ));
        again.unlock("good-master-password").unwrap();
        assert_eq!(again.get("dev").as_deref(), Some("s3cr3t café"));
        again.delete("dev").unwrap();
        assert!(!again.has("dev"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn tampering_is_detected() {
        let (mut v, path) = temp_vault();
        v.create("good-master-password").unwrap();
        v.set("dev", "abc").unwrap();
        let file = v.file.as_mut().unwrap();
        let sealed = file.entries.get_mut("dev").unwrap();
        let mut data = B64.decode(&sealed.data).unwrap();
        data[0] ^= 1;
        sealed.data = B64.encode(data);
        assert_eq!(v.get("dev"), None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A vault file damaged by hand or by a half-written save: unlocking says so, and nothing
    /// panics on the way (an empty or short salt, bad base64, a nonce of the wrong size).
    #[test]
    fn damaged_vault_files_are_errors() {
        let (_, path) = temp_vault();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let sealed = r#"{"nonce": "AAAAAAAAAAAAAAAA", "data": "AAAA"}"#;
        for (salt, check) in [
            ("", sealed),
            ("AA==", sealed),
            ("not base64 !", sealed),
            ("AAAAAAAAAAAAAAAAAAAAAA==", r#"{"nonce": "AA==", "data": "AAAA"}"#),
            ("AAAAAAAAAAAAAAAAAAAAAA==", r#"{"nonce": "", "data": ""}"#),
            ("AAAAAAAAAAAAAAAAAAAAAA==", sealed),
        ] {
            let body = format!(r#"{{"salt": "{salt}", "check": {check}, "entries": {{"dev": {sealed}}}}}"#);
            std::fs::write(&path, body).unwrap();
            let mut v = Vault::load(Some(path.clone()));
            assert!(v.exists(), "{salt}");
            assert!(v.unlock("good-master-password").is_err(), "{salt}");
            assert_eq!(v.get("dev"), None);
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
