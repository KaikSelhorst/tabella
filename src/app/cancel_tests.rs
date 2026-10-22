//! Stopping work through the app, on a real SQLite session: a statement that never ends on its
//! own, and the search, export and script tabs waiting behind it.

use std::time::{Duration, Instant};

use super::*;

/// Never finishes unless interrupted. Reads a table, so the file stays locked while it runs (see
/// `wait_until_running`).
const FOREVER: &str = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) \
                       SELECT count(*) FROM c, (SELECT 1 FROM customer LIMIT 1)";

/// Waits for a statement to be executing on the file, not just taken by the worker: a cancel
/// can only stop what already runs, and one landing in between is lost (a user would click
/// again). Executing, it holds a read lock that keeps another connection from taking the file.
fn wait_until_running(cfg: &ConnConfig) {
    let probe = rusqlite::Connection::open(&cfg.database).unwrap();
    probe.busy_timeout(Duration::ZERO).unwrap();
    let started = Instant::now();
    while probe.execute_batch("BEGIN EXCLUSIVE; ROLLBACK").is_ok() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the statement never started"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn connected(tag: &str) -> (App, ConnConfig) {
    let dir = std::env::temp_dir().join(format!("tabella-cancel-tests-{tag}-{}", std::process::id()));
    crate::store::use_dir_in_this_thread(dir);
    let mut app = App::new(super::window::NoWindow);
    let cfg = crate::sqlite::tests::sample_db(&format!("cancel-{tag}"));
    app.connect_cfg(cfg.clone());
    poll_until(&mut app, "connected", |app| {
        app.session.as_ref().is_some_and(|s| s.ready)
    });
    (app, cfg)
}

fn poll_until(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
    let started = Instant::now();
    while !done(app) {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "still waiting: {what}"
        );
        std::thread::sleep(Duration::from_millis(5));
        app.poll();
    }
}

fn tab_index(app: &App, id: u64) -> usize {
    app.tabs.iter().position(|t| t.id == id).unwrap()
}

/// Opens an editor tab running `sql`; returns the tab's id.
fn run_in_editor(app: &mut App, sql: &str) -> u64 {
    app.open_sql(sql.into());
    let i = app.tabs.len() - 1;
    let s = app.session.as_ref().unwrap();
    run::run_sql(&mut app.tabs[i], s, sql.into());
    app.tabs[i].id
}

#[test]
fn stopped_work_ends_and_the_session_goes_on() {
    let (mut app, cfg) = connected("queue");
    let blocker = run_in_editor(&mut app, FOREVER);

    // Waiting in line behind it: a search, an export and a script.
    app.open_search();
    let search = app.tabs.last().unwrap().id;
    if let TabKind::Search(st) = &mut app.tabs.last_mut().unwrap().kind {
        st.term = "beth".into();
    }
    app.open_dump(DumpScope::Table("main".into(), "customer".into()));
    let dump = app.tabs.last().unwrap().id;
    let script_file = std::env::temp_dir().join(format!("tabella-cancel-{}.sql", std::process::id()));
    std::fs::write(&script_file, "DELETE FROM purchase;\nDELETE FROM customer;\n").unwrap();
    app.open_script(&script_file);
    let script = app.tabs.last().unwrap().id;
    {
        let s = app.session.as_ref().unwrap();
        for (id, start) in [
            (search, jobs::start_search as fn(&mut Tab, &Session)),
            (dump, jobs::start_dump),
            (script, jobs::start_script),
        ] {
            let i = app.tabs.iter().position(|t| t.id == id).unwrap();
            start(&mut app.tabs[i], s);
        }
        for (id, cancel) in [
            (search, jobs::cancel_search as fn(&mut Tab, &Session)),
            (dump, jobs::cancel_dump),
            (script, jobs::cancel_script),
        ] {
            let i = app.tabs.iter().position(|t| t.id == id).unwrap();
            cancel(&mut app.tabs[i], s);
        }
        wait_until_running(&cfg);
        s.cancel(blocker);
    }
    poll_until(&mut app, "every tab answered", |app| {
        !app.tabs.iter().any(Tab::busy)
    });

    let i = tab_index(&app, blocker);
    assert!(app.tabs[i].error.is_some(), "the endless statement was stopped");
    let i = tab_index(&app, search);
    let TabKind::Search(st) = &app.tabs[i].kind else {
        unreachable!()
    };
    assert!(!st.running && st.hits.is_empty());
    let i = tab_index(&app, dump);
    let TabKind::Dump(st) = &app.tabs[i].kind else {
        unreachable!()
    };
    assert!(
        !st.running
            && st
                .outcome
                .as_ref()
                .is_some_and(|o| o.as_ref().is_err_and(|e| e == "cancelled"))
    );
    let i = tab_index(&app, script);
    let TabKind::Script(st) = &app.tabs[i].kind else {
        unreachable!()
    };
    assert!(!st.running && st.outcome.as_ref().is_some_and(Option::is_some));

    // Nothing of the script ran, and the session still answers.
    let check = run_in_editor(&mut app, "SELECT count(*) FROM customer");
    poll_until(&mut app, "the next statement", |app| {
        !app.tabs[tab_index(app, check)].busy()
    });
    let i = tab_index(&app, check);
    assert_eq!(
        app.tabs[i].result.as_ref().and_then(|r| r.rows.cell(0, 0)),
        Some("2")
    );
    let _ = std::fs::remove_file(&script_file);
    let _ = std::fs::remove_file(&cfg.database);
}

/// Quitting while a statement runs stops it on the database instead of leaving it running.
#[test]
fn quitting_stops_what_runs() {
    let (mut app, cfg) = connected("quit");
    let blocker = run_in_editor(&mut app, FOREVER);
    // It's the statement running, not one waiting in line, that must be stopped.
    wait_until_running(&cfg);
    let s = app.session.take().unwrap();
    let started = Instant::now();
    s.cancel_all_and_wait(Duration::from_secs(2));
    // Other replies may come first (the transaction state, say): the statement's is looked for.
    let stopped = loop {
        let left = Duration::from_secs(5).saturating_sub(started.elapsed());
        let reply = s.rx.recv_timeout(left).expect("the statement ended");
        if let worker::Reply::Rows { tab, res, .. } = reply.reply
            && tab == blocker
        {
            break res;
        }
    };
    assert!(stopped.is_err(), "interrupted, not finished");

    // `shutdown` with a busy tab returns promptly too.
    app.session = Some(s);
    let i = tab_index(&app, blocker);
    app.tabs[i].pending = 1;
    let started = Instant::now();
    app.shutdown();
    assert!(app.session.is_none() && started.elapsed() < Duration::from_secs(5));
    let _ = std::fs::remove_file(&cfg.database);
}
