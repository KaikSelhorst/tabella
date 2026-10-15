//! Saved connections. The JSON file never holds passwords: they live in the secret store
//! (see `secrets`).

use std::path::Path;

use crate::db::ConnConfig;
use crate::secrets::SecretStore;
use crate::store::StoreError;

const FILE: &str = "connections.json";

/// A random id for a new connection.
pub fn new_id() -> String {
    let mut bytes = [0u8; 8];
    // The OS RNG only fails on broken systems; a time-based id is still unique enough here.
    if getrandom::fill(&mut bytes).is_err() {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        bytes = (t.as_nanos() as u64).to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Names identify connections for people; two with the same name would be confusing.
pub fn name_taken(list: &[ConnConfig], name: &str, except_id: Option<&str>) -> bool {
    let name = name.trim();
    list.iter()
        .any(|c| Some(c.id.as_str()) != except_id && c.name.trim().eq_ignore_ascii_case(name))
}

/// What `load` found.
pub struct Loaded {
    pub list: Vec<ConnConfig>,
    /// The file needs rewriting: plain-text passwords from older versions, or connections that
    /// had no id yet.
    pub outdated: bool,
    /// The file existed but could not be used (see `store::load_at`).
    pub warning: Option<String>,
}

/// Saved connections, with passwords filled in from the secret store when it can be read now.
pub fn load(store: &SecretStore) -> Loaded {
    let crate::store::Loaded {
        value: mut list,
        warning,
    } = crate::store::load::<Vec<ConnConfig>>(FILE);
    let mut outdated = list.iter().any(|c| !c.password.is_empty());
    for c in &mut list {
        if c.id.is_empty() {
            c.id = new_id();
            outdated = true;
        }
    }
    fill_passwords(&mut list, store);
    Loaded {
        list,
        outdated,
        warning,
    }
}

/// Fills empty passwords from the store (e.g. right after the vault is unlocked). Older versions
/// stored them under the connection name; those are picked up and re-keyed on the next save.
pub fn fill_passwords(list: &mut [ConnConfig], store: &SecretStore) {
    for c in list {
        if c.password.is_empty() {
            c.password = store
                .get(&c.id)
                .or_else(|| store.get(&c.name))
                .unwrap_or_default();
        }
    }
}

/// How a save went for the passwords (the file itself was written).
#[derive(Debug, Default, PartialEq)]
pub struct Saved {
    /// Passwords the store refused (locked): they stay in memory for this session only.
    pub not_stored: usize,
    /// Old copies the store could not remove.
    pub not_removed: usize,
}

/// Writes the connections without passwords; those go to the secret store.
pub fn save(list: &[ConnConfig], store: &mut SecretStore) -> Result<Saved, StoreError> {
    save_at(&crate::store::path(FILE)?, list, store)
}

fn save_at(path: &Path, list: &[ConnConfig], store: &mut SecretStore) -> Result<Saved, StoreError> {
    let mut report = Saved::default();
    let stripped: Vec<ConnConfig> = list
        .iter()
        .cloned()
        .map(|mut c| {
            if c.id.is_empty() {
                c.id = new_id();
            }
            if c.password.is_empty() {
                if store.delete(&c.id).is_err() {
                    report.not_removed += 1;
                }
            } else if store.set(&c.id, &c.password).is_err() {
                report.not_stored += 1;
            }
            // Drop any copy an older version kept under the name.
            if store.delete(&c.name).is_err() {
                report.not_removed += 1;
            }
            // Never written to disk in plain text, whatever happened above.
            c.password.clear();
            c
        })
        .collect();
    crate::store::save_at(path, &stripped)?;
    Ok(report)
}

/// Removes a deleted connection's password (under its id, and its name for older versions).
pub fn forget(cfg: &ConnConfig, store: &mut SecretStore) -> Result<(), crate::secrets::SecretError> {
    store.delete(&cfg.id)?;
    store.delete(&cfg.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Kind;

    #[test]
    fn ids_and_names() {
        let a = new_id();
        assert_eq!(a.len(), 16);
        assert_ne!(a, new_id());
        let mut c = ConnConfig::new(Kind::Postgres);
        c.id = a.clone();
        c.name = "Prod".into();
        let list = [c];
        assert!(name_taken(&list, " prod ", None));
        assert!(!name_taken(&list, "prod", Some(&a)), "editing keeps its own name");
        assert!(!name_taken(&list, "dev", None));
    }

    #[test]
    fn passwords_never_reach_the_file() {
        let dir = std::env::temp_dir().join(format!("dblite-config-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = SecretStore::vault_at(dir.join("vault.json"));
        let mut c = ConnConfig::new(Kind::Postgres);
        c.id = new_id();
        c.name = "prod".into();
        c.password = "hunter2-secret".into();
        let file = dir.join("connections.json");
        let save_to = |list: &[ConnConfig], store: &mut SecretStore| save_at(&file, list, store);

        // Locked vault: the connection is saved, the password is not (and not in plain text).
        assert_eq!(save_to(&[c.clone()], &mut store).unwrap().not_stored, 1);
        assert!(!std::fs::read_to_string(&file).unwrap().contains("hunter2"));

        // With the vault created, the password goes there, still not into the file.
        store.vault_mut().unwrap().create("good-master-password").unwrap();
        assert_eq!(save_to(&[c.clone()], &mut store).unwrap(), Saved::default());
        assert!(!std::fs::read_to_string(&file).unwrap().contains("hunter2"));
        assert!(
            !std::fs::read_to_string(dir.join("vault.json"))
                .unwrap()
                .contains("hunter2")
        );

        // A fresh start reads it back only after unlocking.
        let mut again = SecretStore::vault_at(dir.join("vault.json"));
        let mut list: Vec<ConnConfig> =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        fill_passwords(&mut list, &again);
        assert_eq!(list[0].password, "");
        again.vault_mut().unwrap().unlock("good-master-password").unwrap();
        fill_passwords(&mut list, &again);
        assert_eq!(list[0].password, "hunter2-secret");
        assert_eq!(list[0].id, c.id);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
