//! What the server is doing: sessions, slow queries, plans, and stopping them.

use super::*;

impl Conn {
    /// One of the activity monitor's views. In every view where something can be cancelled,
    /// the first column is the session id to act on.
    pub fn activity_view(&mut self, view: ActivityView) -> DbResult<QueryResult> {
        if self.kind() == Kind::Sqlite {
            return Err(crate::sqlite::no_server("the activity monitor"));
        }
        match view {
            ActivityView::Sessions | ActivityView::Server => self.activity(),
            ActivityView::Locks => self.query(match self.kind() {
                // Who waits for whom: the blocking session first (that's the one to stop).
                Kind::Postgres => {
                    "SELECT b.pid, b.usename AS username, b.state AS state, left(b.query, 200) AS blocking_query, \
                     w.pid AS waiting_pid, date_trunc('second', now() - w.query_start)::text AS waiting_for, \
                     left(w.query, 200) AS waiting_query \
                     FROM pg_stat_activity w \
                     JOIN LATERAL unnest(pg_blocking_pids(w.pid)) AS x(pid) ON true \
                     JOIN pg_stat_activity b ON b.pid = x.pid \
                     ORDER BY w.query_start"
                }
                Kind::MySql | Kind::Sqlite => {
                    "SELECT blocking_pid AS pid, blocking_query, \
                     waiting_pid, wait_age AS waiting_for, waiting_query \
                     FROM sys.innodb_lock_waits ORDER BY wait_age_secs DESC"
                }
            }),
            ActivityView::Slow => {
                let r = self.query(match self.kind() {
                    Kind::Postgres => {
                        "SELECT calls, round(total_exec_time::numeric, 1) AS total_ms, \
                         round(mean_exec_time::numeric, 2) AS mean_ms, rows AS row_count, left(query, 500) AS query \
                         FROM pg_stat_statements \
                         WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) \
                         ORDER BY total_exec_time DESC LIMIT 50"
                    }
                    Kind::MySql | Kind::Sqlite => {
                        "SELECT COUNT_STAR AS calls, ROUND(SUM_TIMER_WAIT / 1e9, 1) AS total_ms, \
                         ROUND(AVG_TIMER_WAIT / 1e9, 2) AS mean_ms, SUM_ROWS_EXAMINED AS rows_examined, \
                         SCHEMA_NAME AS db, LEFT(DIGEST_TEXT, 500) AS query \
                         FROM performance_schema.events_statements_summary_by_digest \
                         WHERE DIGEST_TEXT IS NOT NULL ORDER BY SUM_TIMER_WAIT DESC LIMIT 50"
                    }
                });
                r.map_err(|e| match self.kind() {
                    Kind::Postgres if e.to_string().contains("pg_stat_statements") => DbError::Other(
                        "Slow queries use the pg_stat_statements extension: add it to \
                         shared_preload_libraries (postgresql.conf), restart the server and run \
                         CREATE EXTENSION pg_stat_statements; in the database."
                            .into(),
                    ),
                    _ => e,
                })
            }
        }
    }

    /// The "Server" panel: headline numbers, biggest tables, indexes nobody uses.
    pub fn server_info(&mut self) -> DbResult<ServerInfo> {
        if self.kind() == Kind::Sqlite {
            return Err(crate::sqlite::no_server("the server panel"));
        }
        let (facts_sql, largest, indexes) = match self.kind() {
            Kind::Postgres => (
                "SELECT current_setting('server_version'), \
                 date_trunc('second', now() - pg_postmaster_start_time())::text, \
                 pg_size_pretty(pg_database_size(current_database())), \
                 (SELECT count(*) FROM pg_stat_activity)::text || ' of ' || current_setting('max_connections'), \
                 (SELECT round(100.0 * sum(blks_hit) / nullif(sum(blks_hit) + sum(blks_read), 0), 2)::text || '%' \
                  FROM pg_stat_database)",
                "SELECT n.nspname || '.' || c.relname AS table_name, pg_size_pretty(pg_total_relation_size(c.oid)) AS total, \
                 pg_size_pretty(pg_relation_size(c.oid)) AS data_size, pg_size_pretty(pg_indexes_size(c.oid)) AS index_size, \
                 c.reltuples::bigint AS est_rows \
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE c.relkind IN ('r', 'p', 'm') AND n.nspname NOT IN ('pg_catalog', 'information_schema') \
                 AND n.nspname NOT LIKE 'pg_toast%' \
                 ORDER BY pg_total_relation_size(c.oid) DESC LIMIT 10",
                "SELECT s.schemaname || '.' || s.relname AS table_name, s.indexrelname AS index_name, \
                 s.idx_scan AS scans, pg_size_pretty(pg_relation_size(s.indexrelid)) AS size, \
                 CASE WHEN EXISTS (SELECT 1 FROM pg_constraint c WHERE c.contype = 'f' \
                   AND c.conrelid = i.indrelid AND c.conkey[1] = i.indkey[0]) THEN 'yes' ELSE '' END \
                   AS backs_foreign_key \
                 FROM pg_stat_user_indexes s JOIN pg_index i ON i.indexrelid = s.indexrelid \
                 WHERE NOT i.indisunique AND NOT i.indisprimary \
                 ORDER BY s.idx_scan, pg_relation_size(s.indexrelid) DESC LIMIT 20",
            ),
            Kind::MySql | Kind::Sqlite => (
                "SELECT VERSION(), \
                 (SELECT CONCAT(FLOOR(VARIABLE_VALUE / 86400), 'd ', SEC_TO_TIME(VARIABLE_VALUE % 86400)) \
                  FROM performance_schema.global_status WHERE VARIABLE_NAME = 'Uptime'), \
                 (SELECT CONCAT(ROUND(SUM(data_length + index_length) / 1048576, 1), ' MB') \
                  FROM information_schema.tables WHERE table_schema = DATABASE()), \
                 CONCAT((SELECT VARIABLE_VALUE FROM performance_schema.global_status WHERE VARIABLE_NAME = 'Threads_connected'), \
                  ' of ', @@max_connections), \
                 (SELECT CONCAT(ROUND(100 * (1 - r.VARIABLE_VALUE / NULLIF(q.VARIABLE_VALUE, 0)), 2), '%') \
                  FROM performance_schema.global_status r, performance_schema.global_status q \
                  WHERE r.VARIABLE_NAME = 'Innodb_buffer_pool_reads' AND q.VARIABLE_NAME = 'Innodb_buffer_pool_read_requests')",
                "SELECT CONCAT(table_schema, '.', table_name) AS table_name, \
                 CONCAT(ROUND((data_length + index_length) / 1048576, 1), ' MB') AS total, \
                 CONCAT(ROUND(data_length / 1048576, 1), ' MB') AS data_size, \
                 CONCAT(ROUND(index_length / 1048576, 1), ' MB') AS index_size, table_rows AS est_rows \
                 FROM information_schema.tables \
                 WHERE table_schema NOT IN ('mysql', 'sys', 'performance_schema', 'information_schema') \
                 AND table_type = 'BASE TABLE' \
                 ORDER BY data_length + index_length DESC LIMIT 10",
                // performance_schema only knows tables opened since its counters started: an
                // index with no row there shows NULL ("no data"), not 0.
                "SELECT CONCAT(s.table_schema, '.', s.table_name) AS table_name, s.index_name AS index_name, \
                 u.COUNT_READ AS scans, \
                 IF(EXISTS (SELECT 1 FROM information_schema.key_column_usage k \
                   WHERE k.table_schema = s.table_schema AND k.table_name = s.table_name \
                   AND k.column_name = s.column_name AND k.referenced_table_name IS NOT NULL), 'yes', '') \
                   AS backs_foreign_key \
                 FROM information_schema.statistics s \
                 LEFT JOIN performance_schema.table_io_waits_summary_by_index_usage u \
                   ON u.OBJECT_SCHEMA = s.table_schema AND u.OBJECT_NAME = s.table_name AND u.INDEX_NAME = s.index_name \
                 WHERE s.seq_in_index = 1 AND s.non_unique = 1 \
                 AND s.table_schema NOT IN ('mysql', 'sys', 'performance_schema', 'information_schema') \
                 ORDER BY u.COUNT_READ IS NOT NULL, u.COUNT_READ, s.table_name LIMIT 20",
            ),
        };
        let facts = self.query(facts_sql)?;
        let labels = [
            "Version",
            "Uptime",
            "Database size",
            "Connections",
            "Cache hit rate",
        ];
        let values = facts.rows.into_iter().next().unwrap_or_default();
        Ok(ServerInfo {
            facts: labels
                .iter()
                .zip(values)
                // Postgres prints intervals in English ("3 days 04:12:09"): same form as MySQL's.
                .map(|(l, v)| {
                    let v = v.unwrap_or_else(|| "—".into());
                    (l.to_string(), v.replace(" days ", "d ").replace(" day ", "d "))
                })
                .collect(),
            largest: self.query(largest)?,
            indexes: self.query(indexes)?,
        })
    }

    /// Clears the "slow queries" counters.
    pub fn reset_query_stats(&mut self) -> DbResult<()> {
        if self.kind() == Kind::Sqlite {
            return Err(crate::sqlite::no_server("query statistics"));
        }
        self.query(match self.kind() {
            Kind::Postgres => "SELECT pg_stat_statements_reset()",
            Kind::MySql | Kind::Sqlite => {
                "TRUNCATE TABLE performance_schema.events_statements_summary_by_digest"
            }
        })
        .map(|_| ())
    }

    /// Sessions on the server right now (excluding this one), most long-running first.
    pub fn activity(&mut self) -> DbResult<QueryResult> {
        if self.kind() == Kind::Sqlite {
            return Err(crate::sqlite::no_server("the session list"));
        }
        self.query(match self.kind() {
            Kind::Postgres => {
                "SELECT pid, usename AS username, datname AS db, state, \
                 date_trunc('second', now() - query_start)::text AS duration, \
                 wait_event_type AS wait, client_addr AS client, left(query, 300) AS query \
                 FROM pg_stat_activity \
                 WHERE pid <> pg_backend_pid() AND backend_type = 'client backend' \
                 ORDER BY state = 'active' DESC, query_start NULLS LAST"
            }
            Kind::MySql | Kind::Sqlite => {
                "SELECT id, user AS username, db, command AS state, \
                 SEC_TO_TIME(time) AS duration, state AS wait, host AS client, LEFT(info, 300) AS query \
                 FROM information_schema.processlist \
                 WHERE id <> CONNECTION_ID() AND command <> 'Daemon' \
                 ORDER BY command = 'Query' DESC, time DESC"
            }
        })
    }

    /// Plan of one statement. Postgres: `EXPLAIN (FORMAT JSON)`; MySQL: `EXPLAIN FORMAT=TREE`, or
    /// `EXPLAIN ANALYZE` (8.0.18+). With `analyze` the statement really runs: callers only allow
    /// it for reads.
    pub fn explain(&mut self, sql: &str, analyze: bool) -> DbResult<Plan> {
        if self.kind() == Kind::Sqlite {
            return Err(DbError::Other(
                "The visual plan isn't available for SQLite yet: run EXPLAIN QUERY PLAN in the editor".into(),
            ));
        }
        let sql = sql.trim().trim_end_matches(';');
        let (q, kind) = match (self.kind(), analyze) {
            (Kind::Postgres, false) => (format!("EXPLAIN (FORMAT JSON) {sql}"), Kind::Postgres),
            (Kind::Postgres, true) => (format!("EXPLAIN (ANALYZE, FORMAT JSON) {sql}"), Kind::Postgres),
            (Kind::MySql | Kind::Sqlite, false) => (format!("EXPLAIN FORMAT=TREE {sql}"), Kind::MySql),
            (Kind::MySql | Kind::Sqlite, true) => (format!("EXPLAIN ANALYZE {sql}"), Kind::MySql),
        };
        let r = self.query(&q)?;
        // Postgres returns one JSON cell; MySQL one text cell (or one line per row on old servers).
        let text: Vec<&str> = r.rows.iter().filter_map(|row| row.first()?.as_deref()).collect();
        let text = text.join("\n");
        match kind {
            Kind::Postgres => plan::from_pg_json(&text),
            Kind::MySql | Kind::Sqlite => plan::from_mysql_tree(&text),
        }
        .map_err(DbError::Other)
    }

    /// Stops another session's statement (`terminate` = close that whole connection).
    pub fn kill(&mut self, id: i64, terminate: bool) -> DbResult<()> {
        if self.kind() == Kind::Sqlite {
            return Err(crate::sqlite::no_server("cancelling another session"));
        }
        let sql = match (self.kind(), terminate) {
            (Kind::Postgres, false) => format!("SELECT pg_cancel_backend({id})"),
            (Kind::Postgres, true) => format!("SELECT pg_terminate_backend({id})"),
            (Kind::MySql | Kind::Sqlite, false) => format!("KILL QUERY {id}"),
            (Kind::MySql | Kind::Sqlite, true) => format!("KILL {id}"),
        };
        let r = self.query(&sql)?;
        // Postgres answers `false` instead of failing when the session is gone or belongs to
        // someone this user may not signal.
        if r.rows
            .first()
            .and_then(|row| row.first())
            .is_some_and(|v| v.as_deref() == Some("f"))
        {
            return Err(DbError::Other(format!(
                "session {id} wasn't {} (it already ended, or permission is missing: you must be the same \
                 user, a superuser or have the pg_signal_backend role)",
                if terminate { "terminated" } else { "cancelled" }
            )));
        }
        Ok(())
    }
}
