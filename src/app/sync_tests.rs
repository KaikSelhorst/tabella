//! Saved queries and filters across two windows: what one saves or deletes, the other picks up
//! when it gets the focus back, and nothing either did is lost.

use super::tests::fake_session;
use super::*;

fn window(dir: &std::path::Path) -> App {
    crate::store::use_dir_in_this_thread(dir.to_path_buf());
    let mut app = App::new(super::window::NoWindow);
    app.connections.secrets = SecretStore::vault_at(dir.join("vault.json"));
    let (s, _jobs, _replies) = fake_session(Kind::Postgres);
    app.session = Some(s);
    app
}

/// Each window is its own process with its own memory of the files; these tests share one, so
/// before a window acts it gets back what it last saw.
struct Seen {
    favorites: Vec<Favorite>,
    filters: Vec<SavedFilter>,
}

impl Seen {
    fn of(app: &App) -> Self {
        Seen {
            favorites: app.library.favorites.clone(),
            filters: app.library.filters.clone(),
        }
    }

    fn restore(&self) {
        let path = |name| crate::store::path(name).unwrap();
        crate::store::last_seen_by_this_window(&path("favorites.json"), &self.favorites);
        crate::store::last_seen_by_this_window(&path("filters.json"), &self.filters);
    }
}

fn names(app: &App) -> Vec<&str> {
    let mut v: Vec<&str> = app.library.favorites.iter().map(|f| f.name.as_str()).collect();
    v.sort_unstable();
    v
}

#[test]
fn two_windows_share_saved_queries_and_filters() {
    let dir = std::env::temp_dir().join(format!("tabella-sync-tests-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut a = window(&dir);
    let mut b = window(&dir);
    let b_seen = Seen::of(&b);

    a.add_favorite(FavPrompt {
        name: " everywhere ".into(),
        sql: "SELECT 1".into(),
        shared: true,
    });
    a.add_favorite(FavPrompt {
        name: "here only".into(),
        sql: "SELECT 2".into(),
        shared: false,
    });
    a.save_filter("public", "orders", "open", " status = 'open' ");
    let a_seen = Seen::of(&a);
    let fav = |app: &App, name: &str| {
        app.library
            .favorites
            .iter()
            .find(|f| f.name == name)
            .cloned()
            .unwrap()
    };
    assert_eq!(fav(&a, "everywhere").conn, "", "trimmed, and on every connection");
    assert_eq!(fav(&a, "here only").conn, "t");
    assert_eq!(a.library.filters[0].filter, "status = 'open'");

    // Typing in b means focusing it first, which picks up what a saved.
    b_seen.restore();
    b.sync_from_disk();
    assert_eq!(names(&b), ["everywhere", "here only"], "b takes in a's");
    b.add_favorite(FavPrompt {
        name: "from b".into(),
        sql: "SELECT 3".into(),
        shared: true,
    });
    // Saving a filter under a name it already has replaces it.
    b.save_filter("public", "orders", "open", "status = 'open' AND total > 0");
    assert_eq!(b.library.filters.len(), 1);
    let b_seen = Seen::of(&b);

    a_seen.restore();
    a.sync_from_disk();
    assert_eq!(names(&a), ["everywhere", "from b", "here only"], "a picks up b's");
    assert_eq!(a.library.filters[0].filter, "status = 'open' AND total > 0");
    let id = fav(&a, "everywhere").id;
    a.delete_favorite(&id);
    let filter = a.library.filters[0].id.clone();
    a.delete_filter(&filter);

    b_seen.restore();
    b.sync_from_disk();
    assert_eq!(names(&b), ["from b", "here only"], "deleted in a, gone in b");
    assert!(b.library.filters.is_empty());
    assert!(b.notice.as_ref().is_none_or(|n| !n.error));

    let fresh = window(&dir);
    assert_eq!(names(&fresh), ["from b", "here only"]);
    assert!(fresh.library.filters.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
