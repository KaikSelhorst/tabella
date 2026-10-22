//! Saved connections through the app: the master password prompt, where passwords end up, and
//! what deleting, importing and backing up do.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::tests::fake_session;
use super::*;

/// A config folder of its own (tests run side by side) and passwords in a vault there.
fn app_in(tag: &str) -> (App, PathBuf) {
    let dir = std::env::temp_dir().join(format!("tabella-conn-tests-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    crate::store::use_dir_in_this_thread(dir.clone());
    (reopen(&dir), dir)
}

/// The app started again on the same folder (a new window, or the next run).
fn reopen(dir: &std::path::Path) -> App {
    crate::store::use_dir_in_this_thread(dir.to_path_buf());
    let mut app = App::new(super::window::NoWindow);
    // Never the machine's own keyring.
    app.connections.secrets = SecretStore::vault_at(dir.join("vault.json"));
    app.connections.saved = config::load(&app.connections.secrets).list;
    app
}

fn postgres(name: &str, password: &str) -> ConnConfig {
    ConnConfig {
        name: name.into(),
        host: "127.0.0.1".into(),
        // Nothing listens there: connecting fails at once, which is fine here.
        port: 1,
        password: password.into(),
        ..ConnConfig::new(Kind::Postgres)
    }
}

/// Types `password` into the master prompt and waits for the key derivation.
fn enter_master(app: &mut App, password: &str) {
    let p = app
        .connections
        .master
        .as_mut()
        .expect("the master password prompt");
    p.password = password.into();
    p.confirm = password.into();
    app.start_master_kdf();
    let started = Instant::now();
    while app
        .connections
        .master
        .as_ref()
        .is_some_and(|p| p.working.is_some())
    {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "key derivation hangs"
        );
        std::thread::sleep(Duration::from_millis(10));
        app.finish_master_kdf();
    }
}

fn connections_file(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join("connections.json")).unwrap_or_default()
}

#[test]
fn a_new_password_waits_for_the_vault_and_never_reaches_the_file() {
    let (mut app, dir) = app_in("save");
    app.store_connection(postgres("prod", "hunter2-secret"));
    let id = app.connections.saved[0].id.clone();
    assert!(!id.is_empty());
    let p = app
        .connections
        .master
        .as_ref()
        .expect("asks for a master password first");
    assert!(matches!(
        (&p.mode, &p.then),
        (MasterMode::Create, AfterMaster::Save)
    ));

    // Typed twice, differently: nothing happens.
    let p = app.connections.master.as_mut().unwrap();
    p.password = "good-master-password".into();
    p.confirm = "another-password".into();
    app.start_master_kdf();
    let p = app.connections.master.as_ref().unwrap();
    assert!(p.working.is_none() && p.error.as_deref().is_some_and(|e| e.contains("match")));

    enter_master(&mut app, "good-master-password");
    assert!(app.connections.master.is_none());
    let file = connections_file(&dir);
    assert!(file.contains("prod") && !file.contains("hunter2"), "{file}");
    let vault = std::fs::read_to_string(dir.join("vault.json")).unwrap();
    assert!(vault.contains(&id) && !vault.contains("hunter2"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_saved_password_needs_the_master_password_to_connect() {
    let (mut app, dir) = app_in("connect");
    app.store_connection(postgres("prod", "hunter2-secret"));
    enter_master(&mut app, "good-master-password");
    let id = app.connections.saved[0].id.clone();

    let mut again = reopen(&dir);
    assert_eq!(
        again.connections.saved[0].password.as_str(),
        "",
        "locked: not read yet"
    );
    again.connect_saved(&id);
    assert!(again.session.is_none(), "doesn't connect without the password");
    let p = again.connections.master.as_ref().expect("asks to unlock");
    assert!(matches!(&p.then, AfterMaster::Connect(c) if *c == id));
    assert!(matches!(p.mode, MasterMode::Unlock));

    enter_master(&mut again, "wrong-wrong-wrong");
    let p = again.connections.master.as_ref().expect("still asking");
    assert!(
        p.error.is_some() && p.password.is_empty(),
        "the wrong one is wiped"
    );
    assert!(again.session.is_none());

    enter_master(&mut again, "good-master-password");
    let s = again.session.as_ref().expect("connects once unlocked");
    assert_eq!(s.cfg.password.as_str(), "hunter2-secret");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deleting_a_connection_deletes_its_password() {
    let (mut app, dir) = app_in("delete");
    app.store_connection(postgres("prod", "prod-secret"));
    enter_master(&mut app, "good-master-password");
    app.store_connection(postgres("dev", "dev-secret"));
    let (prod, dev) = (
        app.connections.saved[0].id.clone(),
        app.connections.saved[1].id.clone(),
    );

    app.delete_connection(&prod);
    assert_eq!(app.connections.saved.len(), 1);
    let vault = std::fs::read_to_string(dir.join("vault.json")).unwrap();
    assert!(!vault.contains(&prod) && vault.contains(&dev), "{vault}");
    assert!(!connections_file(&dir).contains("prod"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn editing_a_connection_keeps_its_id_and_password() {
    let (mut app, dir) = app_in("edit");
    app.store_connection(postgres("prod", "prod-secret"));
    enter_master(&mut app, "good-master-password");
    let mut edited = app.connections.saved[0].clone();
    edited.name = "production".into();
    app.store_connection(edited.clone());
    assert_eq!(app.connections.saved.len(), 1, "replaced, not added");
    assert_eq!(app.connections.saved[0].id, edited.id);

    let mut again = reopen(&dir);
    again
        .connections
        .secrets
        .vault_mut()
        .unwrap()
        .unlock("good-master-password")
        .unwrap();
    again.after_master(AfterMaster::Nothing);
    let c = &again.connections.saved[0];
    assert_eq!(
        (c.name.as_str(), c.password.as_str()),
        ("production", "prod-secret")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exported_connections_import_without_passwords() {
    let (mut app, dir) = app_in("export");
    let path = dir.join("shared.json");
    app.export_connections_dialog();
    assert!(
        app.notice.as_ref().is_some_and(|n| n.error),
        "nothing to export yet"
    );
    app.store_connection(postgres("prod", "prod-secret"));
    enter_master(&mut app, "good-master-password");
    app.export_connections(&path);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("prod") && !text.contains("prod-secret"), "{text}");

    let (mut other, other_dir) = app_in("import");
    other.import_connections(&path);
    assert!(
        other.notice.as_ref().is_some_and(|n| !n.error),
        "{:?}",
        other.notice.as_ref().map(|n| &n.text)
    );
    let c = &other.connections.saved[0];
    assert!(c.name == "prod" && c.password.is_empty() && c.id != app.connections.saved[0].id);
    assert!(connections_file(&other_dir).contains("prod"), "saved right away");

    other.import_connections(&other_dir.join("missing.json"));
    assert!(other.notice.as_ref().is_some_and(|n| n.error));
    assert_eq!(other.connections.saved.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&other_dir);
}

/// A backup reports how it ended, and only one runs at a time.
#[test]
fn a_backup_reports_its_outcome() {
    let (mut app, dir) = app_in("backup");
    let (mut s, _jobs, _replies) = fake_session(Kind::Postgres);
    // Not something the tools can do: fails before starting any.
    s.cfg.ssh = Some(crate::db::SshConfig {
        host: "bastion".into(),
        port: 22,
        user: "me".into(),
        key_path: String::new(),
    });
    app.session = Some(s);
    app.start_backup(&dir.join("shop.dump"), false);
    app.start_backup(&dir.join("other.dump"), false);
    assert!(
        app.notice
            .as_ref()
            .is_some_and(|n| n.error && n.text.contains("already running"))
    );
    let started = Instant::now();
    while app.connections.backup.is_some() {
        assert!(started.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(10));
        app.poll_backup();
    }
    let n = app.notice.as_ref().unwrap();
    assert!(n.error && n.text.contains("SSH"), "{}", n.text);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_new_database_file_never_replaces_one() {
    let (_, dir) = app_in("newdb");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("new.db");
    create_database_file(&path).unwrap();
    assert!(create_database_file(&path).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
