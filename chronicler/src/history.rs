use anyhow::{Context, Error};
use futures::{Stream, TryStreamExt};
use serde::{Deserialize, Serialize};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode},
    FromRow, Row, SqlitePool,
};
use std::path::Path;
use std::{fmt::Write, str::FromStr};
use sweep::{
    surf_n_term::view::{Align, Container, Frame, View},
    Haystack, Theme,
};

#[derive(Clone, Debug, FromRow)]
pub struct HistoryEntry {
    pub id: i64,
    pub cmd: String,
    #[sqlx(rename = "return")]
    pub status: i64,
    pub cwd: String,
    pub hostname: String,
    pub user: String,
    pub start_ts: f64,
    pub end_ts: f64,
    pub session: String,
}

impl Haystack for HistoryEntry {
    fn haystack(&self) -> Box<dyn Iterator<Item = char> + '_> {
        Box::new(self.cmd.chars())
    }

    fn preview(&self, theme: &Theme) -> Option<Box<dyn View>> {
        let mut text = String::new();
        (|| {
            writeln!(&mut text, "id: {}", self.id)?;
            writeln!(&mut text, "cmd: {}", self.cmd)?;
            writeln!(&mut text, "status: {}", self.status)?;
            writeln!(&mut text, "cwd: {}", self.cwd)?;
            writeln!(&mut text, "hostname: {}", self.hostname)?;
            writeln!(&mut text, "user: {}", self.user)?;
            writeln!(&mut text, "duration: {}", self.end_ts - self.start_ts)?;
            write!(&mut text, "session: {}", self.session)?;
            Ok::<_, anyhow::Error>(())
        })()
        .expect("in memory write failed");
        Some(
            Frame::new(
                Container::new(text)
                    .with_horizontal(Align::Expand)
                    .with_color(theme.fg),
                theme.fg,
                theme.bg,
                0.2,
                0.7,
            )
            .boxed(),
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct HistoryUpdate {
    pub id: Option<i64>,
    pub cmd: Option<String>,
    pub status: Option<i64>,
    pub cwd: Option<String>,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub start_ts: Option<f64>,
    pub end_ts: Option<f64>,
    pub session: Option<String>,
}

impl FromStr for HistoryUpdate {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut result = HistoryUpdate::default();
        for kv in s.split('\x00') {
            let mut kv = kv.splitn(2, '\n');
            match (kv.next(), kv.next()) {
                (Some("id"), Some(val)) => result.id = Some(val.trim().parse()?),
                (Some("cmd"), Some(val)) => result.cmd = Some(val.to_owned()),
                (Some("status" | "return"), Some(val)) => result.status = Some(val.trim().parse()?),
                (Some("cwd"), Some(val)) => result.cwd = Some(val.trim().to_owned()),
                (Some("hostname"), Some(val)) => result.hostname = Some(val.trim().to_owned()),
                (Some("user"), Some(val)) => result.user = Some(val.trim().to_owned()),
                (Some("start_ts"), Some(val)) => result.start_ts = Some(val.trim().parse()?),
                (Some("end_ts"), Some(val)) => result.end_ts = Some(val.trim().parse()?),
                (Some("session"), Some(val)) => result.session = Some(val.trim().to_owned()),
                (Some(key), _) => return Err(anyhow::anyhow!("invalid key `{key}`")),
                _ => continue,
            }
        }
        Ok(result)
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct PathEntry {
    pub path: String,
    pub count: i64,
}

#[derive(Clone)]
pub struct History {
    pool: SqlitePool,
}

impl History {
    pub async fn new(path: impl AsRef<Path>) -> Result<Self, Error> {
        let options = SqliteConnectOptions::new()
            .journal_mode(SqliteJournalMode::Wal)
            .create_if_missing(true)
            .filename(path)
            .thread_name(|index| format!("hist-sqlite-{index}"));
        let pool = SqlitePool::connect_lazy_with(options);
        sqlx::query(CREATE_TABLE_QUERY)
            .execute(&mut pool.acquire().await?)
            .await?;
        Ok(Self { pool })
    }

    /// Close database, on drop it will be closed anyway but there is not way to
    /// return an error.
    pub async fn close(self) -> Result<(), Error> {
        self.pool.close().await;
        Ok(())
    }

    /// All history entries in the database
    pub fn entries(&self) -> impl Stream<Item = Result<HistoryEntry, Error>> + '_ {
        sqlx::query_as(LIST_QUERY)
            .fetch(&self.pool)
            .map_err(Error::from)
    }

    pub fn path_entries(&self) -> impl Stream<Item = Result<PathEntry, Error>> + '_ {
        sqlx::query_as(PATH_QUERY)
            .fetch(&self.pool)
            .map_err(Error::from)
    }

    /// Update/Create new entry
    ///
    /// New entry is added if id is not specified, otherwise it is updated
    pub async fn update(&self, entry: HistoryUpdate) -> Result<i64, Error> {
        let mut conn = self.pool.acquire().await?;
        match entry.id {
            None => {
                let id = sqlx::query(INSERT_QUERY)
                    .bind(entry.cmd)
                    .bind(entry.status)
                    .bind(entry.cwd)
                    .bind(entry.hostname)
                    .bind(entry.user)
                    .bind(entry.start_ts)
                    .bind(entry.end_ts)
                    .bind(entry.session)
                    .fetch_one(&mut conn)
                    .await
                    .context("insert query")?;
                Ok(id.try_get(0)?)
            }
            Some(id) => {
                sqlx::query(UPDATE_QUERY)
                    .bind(id)
                    .bind(entry.cmd)
                    .bind(entry.status)
                    .bind(entry.cwd)
                    .bind(entry.hostname)
                    .bind(entry.user)
                    .bind(entry.start_ts)
                    .bind(entry.end_ts)
                    .bind(entry.session)
                    .execute(&mut conn)
                    .await
                    .context("update query")?;
                Ok(id)
            }
        }
    }
}

const CREATE_TABLE_QUERY: &str = r#"
-- main history table
CREATE TABLE IF NOT EXISTS history (
    id       INTEGER PRIMARY KEY,
    cmd      TEXT,
    return   INTEGER,
    cwd      TEXT,
    hostname TEXT,
    user     TEXT,
    start_ts REAL,
    end_ts   REAL,
    session  TEXT,
    duration REAL AS (end_ts - start_ts) VIRTUAL
) STRICT;

-- index to speed up retrieval of most frequent paths
CREATE INDEX IF NOT EXISTS history_cwd ON history(cwd);
"#;

const LIST_QUERY: &str = r#"
SELECT * FROM history ORDER BY end_ts DESC;
"#;

const PATH_QUERY: &str = r#"
SELECT cwd as path, COUNT(cwd) as count FROM history GROUP BY cwd ORDER BY COUNT(cwd) DESC;
"#;

const INSERT_QUERY: &str = r#"
INSERT INTO history (cmd, return, cwd, hostname, user, start_ts, end_ts, session)
VALUES (
    $1, -- cmd
    COALESCE($2, -1), -- return
    $3, -- cwd
    $4, -- hostname
    $5, -- user
    $6, -- start_ts
    COALESCE($7, $6), -- end_ts
    $8  -- session
) RETURNING id;
"#;

const UPDATE_QUERY: &str = r#"
UPDATE history SET
    cmd = COALESCE($2, cmd),
    return = COALESCE($3, return),
    cwd = COALESCE($4, cwd),
    hostname = COALESCE($5, hostname),
    user = COALESCE($6, user),
    start_ts = COALESCE($7, start_ts),
    end_ts = COALESCE($8, end_ts),
    session = COALESCE($9, session)
WHERE
    id=$1;
"#;