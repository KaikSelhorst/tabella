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
use serde::{Deserialize, Serialize};

const SERVICE: &str = "dblite";
/// Encrypted with the vault key to tell a wrong master password from a corrupted file.
const CHECK: &[u8] = b"dblite-vault-ok";

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
            SecretStore::Os if cfg!(windows) => "Gerenciador de Credenciais do Windows",
            SecretStore::Os => "chaveiro do sistema",
            SecretStore::Vault(_) => "cofre com senha mestra",
        }
    }

    pub fn get(&self, name: &str) -> Option<String> {
        match self {
            SecretStore::Os => os::get(name),
            SecretStore::Vault(v) => v.get(name),
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

    pub fn delete(&mut self, name: &str) {
        match self {
            SecretStore::Os => os::delete(name),
            SecretStore::Vault(v) => v.delete(name),
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

/// Input for the key derivation, sendable to another thread.
pub struct KdfJob {
    master: String,
    salt: Vec<u8>,
    create: bool,
}

impl KdfJob {
    /// Argon2id: deliberately slow (to resist guessing), so never on the UI thread.
    pub fn run(self) -> Result<Derived, String> {
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
}

fn vault_path() -> Option<PathBuf> {
    crate::config::dir().map(|d| d.join("vault.json"))
}

impl Vault {
    fn load(path: Option<PathBuf>) -> Self {
        let file = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok());
        Vault {
            path,
            file,
            cipher: None,
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

    fn derive(master: &str, salt: &[u8]) -> Result<ChaCha20Poly1305, String> {
        let mut key = [0u8; 32];
        Argon2::default()
            .hash_password_into(master.as_bytes(), salt, &mut key)
            .map_err(|e| format!("falha ao derivar a chave: {e}"))?;
        Ok(ChaCha20Poly1305::new(&Key::<ChaCha20Poly1305>::from(key)))
    }

    /// First half of create/unlock, cheap: checks the input and picks the salt. The slow key
    /// derivation runs in `KdfJob::run`, off the UI thread; `finish` applies its result.
    pub fn kdf_job(&self, master: &str, create: bool) -> Result<KdfJob, String> {
        let salt = if create {
            if master.chars().count() < 8 {
                return Err("Use pelo menos 8 caracteres".into());
            }
            // A random 32-byte value; the key type is just a convenient generator.
            Key::<ChaCha20Poly1305>::generate().to_vec()
        } else {
            let file = self.file.as_ref().ok_or("cofre ainda não foi criado")?;
            B64.decode(&file.salt).map_err(|_| "cofre corrompido")?
        };
        Ok(KdfJob {
            master: master.to_string(),
            salt,
            create,
        })
    }

    pub fn finish(&mut self, derived: Derived) -> Result<(), String> {
        let Derived { cipher, salt, create } = derived;
        if create {
            let check = seal(&cipher, CHECK)?;
            self.file = Some(VaultFile {
                salt: B64.encode(salt),
                check,
                entries: BTreeMap::new(),
            });
            self.cipher = Some(cipher);
            return self.save();
        }
        let file = self.file.as_ref().ok_or("cofre ainda não foi criado")?;
        match open(&cipher, &file.check) {
            Some(v) if v == CHECK => {
                self.cipher = Some(cipher);
                Ok(())
            }
            _ => Err("Senha mestra incorreta".into()),
        }
    }

    /// Sets up a new vault with this master password (blocking; the UI uses `kdf_job`).
    #[cfg(test)]
    pub fn create(&mut self, master: &str) -> Result<(), String> {
        let derived = self.kdf_job(master, true)?.run()?;
        self.finish(derived)
    }

    #[cfg(test)]
    pub fn unlock(&mut self, master: &str) -> Result<(), String> {
        let derived = self.kdf_job(master, false)?.run()?;
        self.finish(derived)
    }

    fn get(&self, name: &str) -> Option<String> {
        let cipher = self.cipher.as_ref()?;
        let sealed = self.file.as_ref()?.entries.get(name)?;
        String::from_utf8(open(cipher, sealed)?).ok()
    }

    fn set(&mut self, name: &str, password: &str) -> Result<(), SecretError> {
        let (Some(cipher), Some(file)) = (&self.cipher, &mut self.file) else {
            return Err(SecretError::Locked);
        };
        let sealed = seal(cipher, password.as_bytes()).map_err(|_| SecretError::Unavailable)?;
        file.entries.insert(name.to_string(), sealed);
        self.save().map_err(|_| SecretError::Unavailable)
    }

    fn delete(&mut self, name: &str) {
        if let Some(f) = &mut self.file
            && f.entries.remove(name).is_some()
        {
            let _ = self.save();
        }
    }

    fn save(&self) -> Result<(), String> {
        let (Some(path), Some(file)) = (&self.path, &self.file) else {
            return Ok(());
        };
        let json = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
        crate::config::write_private(path, &json).map_err(|e| e.to_string())
    }
}

fn seal(cipher: &ChaCha20Poly1305, plain: &[u8]) -> Result<Sealed, String> {
    let nonce = Nonce::generate();
    let data = cipher.encrypt(&nonce, plain).map_err(|_| "falha ao cifrar")?;
    Ok(Sealed {
        nonce: B64.encode(nonce),
        data: B64.encode(data),
    })
}

/// `None` for a wrong key or tampered data (the AEAD tag doesn't match).
fn open(cipher: &ChaCha20Poly1305, sealed: &Sealed) -> Option<Vec<u8>> {
    let nonce = B64.decode(&sealed.nonce).ok()?;
    let nonce = Nonce::try_from(nonce.as_slice()).ok()?;
    cipher
        .decrypt(&nonce, B64.decode(&sealed.data).ok()?.as_ref())
        .ok()
}

#[cfg(any(windows, target_os = "linux"))]
mod os {
    use super::SERVICE;

    fn entry(name: &str) -> Option<keyring::Entry> {
        keyring::Entry::new(SERVICE, name).ok()
    }

    /// On Linux the Secret Service may simply not be running (WSL, servers).
    pub fn available() -> bool {
        if cfg!(windows) {
            return true;
        }
        matches!(
            entry("__dblite_probe__").map(|e| e.get_password()),
            Some(Ok(_) | Err(keyring::Error::NoEntry))
        )
    }

    pub fn set(name: &str, password: &str) -> bool {
        entry(name).is_some_and(|e| e.set_password(password).is_ok())
    }

    pub fn get(name: &str) -> Option<String> {
        entry(name)?.get_password().ok()
    }

    pub fn delete(name: &str) {
        if let Some(e) = entry(name) {
            let _ = e.delete_credential();
        }
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
    pub fn delete(_: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_vault() -> (Vault, PathBuf) {
        let dir = std::env::temp_dir().join(format!("dblite-vault-test-{}", Nonce::generate()[0]));
        let path = dir.join("vault.json");
        let _ = std::fs::remove_file(&path);
        (Vault::load(Some(path.clone())), path)
    }

    #[test]
    fn roundtrip_and_reopen() {
        let (mut v, path) = temp_vault();
        assert!(!v.exists());
        assert_eq!(v.set("dev", "x"), Err(SecretError::Locked));
        assert!(v.create("curta").is_err());
        v.create("senha-mestra-boa").unwrap();
        v.set("dev", "s3cr3t ção").unwrap();
        assert_eq!(v.get("dev").as_deref(), Some("s3cr3t ção"));

        // The file never contains the password.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("s3cr3t"));

        // Reopened from disk: locked until the right master password.
        let mut again = Vault::load(Some(path.clone()));
        assert!(again.exists() && !again.is_unlocked() && again.has("dev"));
        assert_eq!(again.get("dev"), None);
        assert_eq!(
            again.unlock("errada-errada"),
            Err("Senha mestra incorreta".into())
        );
        again.unlock("senha-mestra-boa").unwrap();
        assert_eq!(again.get("dev").as_deref(), Some("s3cr3t ção"));
        again.delete("dev");
        assert!(!again.has("dev"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn tampering_is_detected() {
        let (mut v, path) = temp_vault();
        v.create("senha-mestra-boa").unwrap();
        v.set("dev", "abc").unwrap();
        let file = v.file.as_mut().unwrap();
        let sealed = file.entries.get_mut("dev").unwrap();
        let mut data = B64.decode(&sealed.data).unwrap();
        data[0] ^= 1;
        sealed.data = B64.encode(data);
        assert_eq!(v.get("dev"), None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
