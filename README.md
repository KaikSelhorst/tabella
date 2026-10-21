# tabella

Lightweight database client for **PostgreSQL**, **MySQL** and **SQLite**, written in Rust with
[egui](https://github.com/emilk/egui). A single executable, no runtime.

## Features

- Table browsing with paging, a `WHERE` filter, sorting and the total row count
- Editing directly in the grid and in a row detail panel (tables with a primary key): changes
  stay pending and are saved together in one transaction (`Ctrl+S`), with undo (`Ctrl+Z`)
- SQL editor with syntax highlighting, autocomplete, formatting (`Shift+Alt+F`), history,
  favorite queries and running the selection
- Import a connection by pasting a URL (`postgres://…`, `mysql://…`)
- Reopens the last connection, the tabs (with the editors' text) and the window size
- Command palette (`Ctrl+P`) to open tables, connections and recent queries
- Inserting rows through a form (each column: database default, `NULL` or a value)
- "Structure" tab: columns, indexes, foreign keys and DDL ready to copy
- Export to CSV, Excel, JSON or SQL (`INSERT`): the current page or the whole table (in chunks,
  without loading everything into memory), saved to the Downloads folder
- Copying rows as JSON or `INSERT`
- Cancelling running queries and a per-connection timeout (applied on the server)
- SSH tunnel through the system's `ssh` client (keys or agent)
- A color per connection: a colored band at the top while connected (e.g. red for production)
- Search across all of a table's columns and "filter by this value" (equal/different) in the
  cell menu
- Clickable foreign keys: `Ctrl+click` (or the cell menu) opens the referenced row
- Tables' size on disk in the sidebar, with an option to sort by size
- Activity monitor: server sessions, auto refresh, cancel a query or kill a connection (with
  confirmation)
- Editor: `Ctrl+Enter` runs the statement under the cursor (marked in the margin), `F5` runs
  everything; a script with several queries shows one result for each
- Visual execution plan (`Ctrl+E`, or EXPLAIN ANALYZE for reads only): time/cost of each step,
  with full scans of large tables and bad estimates highlighted
- Multi-cell selection (drag, `Shift`+click/arrows, `Ctrl+A`): copies as tab-separated text
  (pastes into Excel) and shows count, sum, average, minimum and maximum
- Pasting from Excel into the grid (`Ctrl+V`): each cell becomes a pending change
- Importing CSV into a table (button or dragging the file): separator and encoding detected,
  columns matched by name, numbers like `1.520,30` and `yes`/`no` converted, all in one
  transaction
- Manual commit per connection (or just for the session, from the "…" menu): changes stay in a
  transaction until "Commit" or "Roll back", with a warning band and a confirmation when closing
  the window. Grid edits and imports use a savepoint inside it. In MySQL, DDL commits on its own.
  Cells saved from a grid in that transaction stay tinted until it is committed or rolled back
- Parameters in SQL (`WHERE id = :id`): the app asks for the values before running
- Find and replace in the editor (`Ctrl+F` / `Ctrl+H`)
- JSON as a tree in the detail panel (copies a value or its path, e.g. `$.items[0].price`)
- Column statistics (right-click the header): nulls, distinct values, min/max and the most
  common values, with a shortcut to filter
- Pinning columns to the left (they stay visible when scrolling sideways) and hiding columns,
  from the header's right-click menu; remembered per table tab
- Find in the editor's result (`Ctrl+F` in the grid); in tables, `Ctrl+F` goes to the database
  search
- Duplicate row: the insert form already filled in (generated keys stay on default)
- Saved filters per table (funnel before the WHERE)
- Snippets in autocomplete (`sel`, `cnt`, `grp`, `dup`, `join`, `upd`…) and highlighted
  `:parameters`
- Keyboard shortcuts and snippets on one screen (`F1`)
- Creating and altering tables through a form: columns (name, type, null, default), primary key
  and indexes, with the generated SQL in view; opens it in the editor or runs it (with
  confirmation)
- UPDATE/DELETE preview ("Preview" button or `Ctrl+Shift+Enter`; automatic if the connection
  asks for it): how many rows and which ones would be affected, without changing anything
- "Rows pointing to this one" in the cell menu: opens the tables whose foreign keys reference
  the row
- Activity monitor with three views: sessions, locks (who blocks whom) and slow queries
  (`pg_stat_statements` / `performance_schema`), with "Reset statistics"
- ENUM columns (PostgreSQL and MySQL): double-click opens a list of the type's values (and NULL when
  allowed) instead of a text box; the row detail panel and the new-row form use a drop-down. Pasted
  or typed values that aren't in the type are refused before saving
- Quick chart of the editor's result: bars or line, value on hover
- The result of a simple single-table SELECT (with the primary key in the SELECT) is editable in
  the editor itself: double-click, paste, delete, `Ctrl+S` saves and the query runs again
- Index suggestion in the execution plan: filtered full scans of large tables become a
  `CREATE INDEX` ready to open in the editor
- "Server" panel in the activity monitor: version, uptime, size, connections, cache hit rate,
  biggest tables and unused indexes
- Its own icon, and a notification (taskbar button flashing and window title) when something
  that took 10 s or more finishes while the window is in the background
- Columns in `Ctrl+P`: type part of the name ("ssn", "customers.email") to find where it is
- Search a value in all tables (`Ctrl+Shift+F`): only looks at columns where the value would
  fit, skips large tables by default, shows progress, can be cancelled and opens each match
  already filtered
- In the editor, hovering a table name shows its columns; `Ctrl+click` opens it
  (a table opens its data; a view, function, procedure, trigger or sequence opens its code; a
  column, written `alias.col`, `table.col` or bare when the statement reads from one table that
  has it, opens the structure of its table with that column highlighted)
- Tabs: right-click to rename, duplicate, close the others/the ones to the right;
  `Ctrl+Shift+T` reopens the last closed one
- Split view, like in Zed: `Ctrl+\` (or right-click a tab → "Split right") opens a second pane with
  its own tab strip and selected tab; drag tabs between the strips or use "Move to the other
  pane". The pane with the active tab has focus; new tabs open in it and `Ctrl+Tab` cycles its
  tabs. A SQL tab is copied to the new pane, any other tab moves. The split closes when a pane
  runs out of tabs, or from "Close split view", and is restored with the session
- "Repeat": runs the last read query every 2 s … 1 min
- ER diagram per schema (`Ctrl+P` or right-click the schema): tables and foreign keys laid out
  automatically, with zoom, dragging and double-click to open
- Autocomplete understands aliases (`FROM orders o` → `o.` suggests the columns of orders) and,
  after `JOIN`, suggests the join ready-made from the foreign keys
  (`customers c ON c.id = o.customer_id`)
- `.sql` files: open (`Ctrl+O` or drag), save (`Ctrl+S`) and save as (`Ctrl+Shift+S`); the tab
  marks unsaved changes and warns before closing
- Full history (`Ctrl+Shift+H`): search, filter by connection and by error, open in the editor
- Light or dark theme and interface zoom from `Ctrl+P` (zoom also with `Ctrl +`/`Ctrl −`/
  `Ctrl 0`); both are remembered. "Follow the system theme" (same palette; on by default on a
  first run) matches the operating system's light/dark setting where the system reports it
- Row limit for editor results (`Ctrl+P` → "Row limit": 1 000 · 5 000 · 20 000 · 100 000); a
  truncated read shows "Load N more" under the grid
- "Copy values as (…) list" in a column's header menu: the distinct values of the selection (or the
  whole column) as `(1, 2, 3)` / `('a', 'b')`, ready for `IN`
- `Alt+←` (or the mouse's back button, or "Go back" in `Ctrl+P`) returns to where the last foreign
  key was followed from, and restores the filter that table had
- Tabs reorder by dragging along the strip (a line shows where it lands); dropping one on the
  right edge of the content opens the split view with it there
- Command line: `tabella [--conn NAME] [URL] [FILE.sql …]` connects to a saved connection (by name
  or id) or to a `postgres://`/`mysql://`/`sqlite:` URL (not saved) and opens the files; without
  arguments it resumes the last session. `tabella --help` prints the usage
- A dropped connection is replaced before the next query (Postgres notices at once, MySQL after
  a failed query or 30 s idle); the picked schema comes back, and an open transaction is reported
  as lost
- Connection groups ("Group" in the form) list connections together in the welcome screen, the
  sidebar and `Ctrl+P`; "Confirm before any change" makes the editor ask before every statement
  that changes data
- `Ctrl+P` → "Export connections" / "Import connections": a JSON file without passwords (ids are
  new on import, a repeated name gets a number)
- `Ctrl+P` → "Back up database" / "Restore database": runs `pg_dump`/`pg_restore`/`psql` or
  `mysqldump`/`mysql` (they must be installed; the password goes in the environment, not the
  command line). A `.sql` name is plain text; other names are PostgreSQL's compressed format
- "Suggest index" in the Slow queries view: reads the selected query's WHERE, JOIN and ORDER BY
  columns, compares them with the table's indexes and proposes a `CREATE INDEX` (or says an index
  already covers it). It opens in the editor, never runs by itself; it is a guess from the text,
  so check it with EXPLAIN ANALYZE. Queries with OR or subqueries aren't analysed
- `Ctrl+P` lists the tables opened last first ("recent"), per connection, across runs
- Pinned tabs (right-click → "Pin tab"): they stay at the start of their strip, have a pin instead
  of the close button, are left alone by "Close others" and `Ctrl+W`, and come back with the session
- Sort by several columns: click a header to sort by it alone, `Shift+click` adds more (numbers
  in the headers show the order; each goes ascending, descending, then out). Saved with the session
  and used by the export
- Bulk edit by expression: right-click a cell of a table → "Set <column> with an expression…"
  (`price * 1.1`, `upper(name)`, `NULL`…) on the selected rows or on every row matching the
  filter. It opens the UPDATE in an editor and shows how many rows and which ones would change;
  nothing is written until you run it there
- Plans can be compared: "Pin to compare" in the plan view keeps a plan, and the next one is shown
  against it (time, cost, rows, tables read in full, scans that changed), e.g. before and after
  creating an index
- Star tables in the sidebar (right-click → "Star this table"): they are listed first; selecting a
  cell lights the same value elsewhere in its column
- Export → "Copy to clipboard" (CSV, JSON or SQL INSERT): the rows on screen as text, ready to
  paste, without a file; the same text the file export writes. Up to 20 MB
- A script of several statements run from the editor is timed statement by statement: the
  footer's "N statements" lists the line, time and rows of each one (the slowest is highlighted).
  Scripts with BEGIN/COMMIT inside, or over 100 statements, run as a single text
- Column widths dragged in a table are remembered for that table (per connection), across tabs
  and runs
- Functions, procedures, triggers and sequences in the sidebar (views along with the tables):
  one click opens the code in an editor, ready to change and run
- "Generate SQL" in the table's right-click menu: SELECT, INSERT, UPDATE and DELETE by key, with
  `:parameters`
- Bulk editing: set a value or NULL in all selected cells, fill down (`Ctrl+D`) and delete the
  selected rows (`Delete`)
- Export to Excel (.xlsx, with numbers and booleans as such) and copy as a Markdown table
- Switching database/schema without editing the connection (selectors below the connection in
  the sidebar); the editor shows where names without a schema end up
- Running a function/procedure from the right-click menu: the arguments become `:parameters`
  asked for in a form
- Table actions in the right-click menu: rename, duplicate (structure only or with data),
  truncate (`TRUNCATE`) and drop, these two only after typing the table name
- Running a large `.sql` file (`Ctrl+P` or the editor's file menu) without opening it in the
  editor: reads it statement by statement (understands `DELIMITER`, `$$` and pg_dump's
  `COPY … FROM stdin`), shows progress, stops at the first error with its line, can be
  cancelled and runs in a transaction. When it ends, the ten slowest statements are listed with
  their line and time
- SQL errors marked in the editor: the spot is underlined, the cursor goes to it and the
  message gives the line and column
- Excel-style column filter (right-click the header → "Filter values…"): distinct values with
  counts, check/uncheck and search; becomes `IN`/`NOT IN` in the WHERE and the column gets a
  funnel
- Exporting a table, schema or database as `.sql` (structure and/or data, views, functions,
  triggers), in chunks and with progress; loads back with "Run .sql file", psql or mysql
- Comparing structure between schemas or databases (including another connection's, e.g.
  dev × production) and generating the SQL that brings the target in line; removals are
  highlighted and `DROP TABLE` is only commented
- Importing Excel (.xlsx, .xls) and ODS in the same dialog as CSV
- "Test connection" in the form (server and version, or the server's error) without saving; the
  form says what's missing when "Save" is disabled
- Notifications in the corner of the screen: informational ones go away on their own, errors
  stay until closed (with copy)
- Large editor for long values (cell menu, Shift+Enter or ↗ in the detail panel): JSON
  formatted, colored and validated without touching numbers; saved compact again if it came
  compact
- Pinning a result in the editor and comparing it with the next ones: new, removed and changed
  rows (old → new), matched by the chosen key or by position
- Picking a foreign key from a list (Enter/double-click on the cell, or the magnifier in the
  panel and in the new row form): id and a readable name, with search; small tables filtered on
  the spot, large ones searched in the database (pause while typing, 50 results, up to 2 s per
  search)
- SQLite: open a `.db` file (or create a new one, or paste `sqlite:///path`) and use almost
  everything — tables, editor, grid editing, foreign keys, structure, designer, import, `.sql`
  export, structure comparison. What depends on a server (activity, sessions, visual plan) says
  it doesn't exist in SQLite; changing the type/NULL of an existing column shows up as a comment
  (SQLite can't do it)
- The tab bar scrolls (including with the mouse wheel), follows the active tab and lists all
  tabs when they don't fit

## Security

- **Passwords** never go to disk in plain text: Credential Manager (Windows), the system keyring
  via Secret Service (Linux with GNOME Keyring/KWallet) or, without a keyring, a vault encrypted
  with a master password (Argon2id + ChaCha20-Poly1305).
- **Values** in the edits the app generates are parameters (MySQL) or literals escaped
  independently of the server configuration (Postgres).
- **The `WHERE` filter** accepts a single condition (no `;` or `--`).
- **TLS**: Postgres tries TLS by default (`prefer`); "SSL/TLS" requires it and verifies the
  certificate.
- **Read-only** per connection: the session is read-only on the server itself.
- Destructive statements in the editor (`DROP`, `TRUNCATE`, `ALTER`, `DELETE`/`UPDATE` without
  `WHERE`) ask for confirmation.

## Build

```sh
cargo build --release                                      # Linux
cargo build --release --target x86_64-pc-windows-gnu       # Windows (needs mingw-w64)
```

The `dev-conn` feature lists the connections of the sample databases (below). It embeds the test
passwords in the binary: don't use it in distributed builds.

## Releases

Bump `version` in `Cargo.toml`, commit, then publish a GitHub release tagged with that version
(`v0.2.0` for `0.2.0`). The release workflow builds and attaches:

- Windows: `.exe` and a `.zip`
- Linux: `.AppImage`, `.deb` and a `.tar.gz` (built on Ubuntu 22.04, so they run on distros
  with glibc 2.35 or newer)
- a `.sha256` checksum for each file

A tag that doesn't match `Cargo.toml` fails the workflow before anything is built. To make the
Linux packages locally: `cargo install cargo-packager --locked`, then
`cargo build --release && source .github/scripts/appimage-tools.sh && cargo packager --release`
(output in `target/packages`). That script pins the AppImage tools (linuxdeploy, its AppImage
plugin, AppRun and the AppImage runtime) to versioned releases checked by SHA-256, so no
"continuous" build ever gets in; update them there.

## Databases for development and tests

```sh
docker compose --profile dev up -d --wait     # heavy sample data: Postgres :5432, MySQL :3307 (~4 min)
docker compose --profile test up -d --wait    # databases for the integration tests
cargo test                                    # unit and UI tests
cargo test -- --ignored db:: worker:: tunnel::   # tests against the real databases (and SSH)
```

The sample data is large on purpose: in Postgres, `customers` (1M), `orders` (5M), `events`
(300k, a wide table with JSON, arrays and IPs) and 200 small tables in the `archive` schema; in
MySQL, `products` (1M), `sales` (3M), `logs` (200k) and 150 tables in the `archive` database.
The scripts in `dev/` can be run again on an existing database (they recreate what they own).

The `test` profile includes a throwaway SSH server (port 52222) that reaches the test Postgres;
the key in `dev/ssh/` is only for it.

The UI screenshots (fake data, no database) are generated with
`TABELLA_SHOTS=<folder> cargo test screenshots -- --ignored` and need a Vulkan driver
(on WSL: `mesa-vulkan-drivers`).

## Configuration

Files live in `%APPDATA%\tabella` (Windows) or `~/.config/tabella` (Linux); `TABELLA_CONFIG_DIR`
changes the location. `connections.json` holds no passwords.
