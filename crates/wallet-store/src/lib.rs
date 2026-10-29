//! SQLite storage for a `wallet_core::Book`. Load everything at start, then
//! `commit` after each write to persist what the book says changed.
//! Plain columns and indexes, so later analytics can run SQL straight on it.
//!
//! Every version of every record also goes into an append-only `history`
//! table, stamped with the device that made it. Sync swaps history entries
//! between peers; the live tables are always the newest-wins result.

pub mod codec;

use std::collections::BTreeMap;
use std::hash::{BuildHasher, RandomState};
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row, Transaction as Tx, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use wallet_core::{Book, Budget, Change, Date, Theme, Transaction, Wallet};

pub use codec::Rec;
use codec::{kind_from, kind_str};
pub use rusqlite::Error;
pub type Result<T> = rusqlite::Result<T>;

/// Each entry runs once, in order; `PRAGMA user_version` counts how many ran.
/// Never edit a shipped entry, only append.
const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE wallets (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, color TEXT NOT NULL,
  ord INTEGER NOT NULL, theme TEXT,
  updated_at INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE transactions (
  id TEXT PRIMARY KEY, wallet_id TEXT NOT NULL,
  amount INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('expense','income','adjustment')),
  date TEXT NOT NULL,
  note TEXT NOT NULL DEFAULT '',
  counts_toward_budget INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX tx_wallet ON transactions(wallet_id);
CREATE INDEX tx_date ON transactions(date);
CREATE TABLE budgets (
  id TEXT PRIMARY KEY, total INTEGER NOT NULL, start_date TEXT NOT NULL,
  days INTEGER NOT NULL, split TEXT NOT NULL,
  updated_at INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE outbox (seq INTEGER PRIMARY KEY AUTOINCREMENT, tbl TEXT NOT NULL, id TEXT NOT NULL);
"#,
    r#"
DROP TABLE outbox;
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE history (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  tbl TEXT NOT NULL, id TEXT NOT NULL, updated_at INTEGER NOT NULL,
  device TEXT NOT NULL,                 -- who made this version
  via TEXT NOT NULL,                    -- who handed it to us (ourselves for local writes)
  data TEXT NOT NULL,                   -- the record as JSON
  UNIQUE (tbl, id, updated_at, device)
);
CREATE INDEX history_record ON history(tbl, id);
CREATE TABLE peers (
  device TEXT PRIMARY KEY, node TEXT NOT NULL UNIQUE, name TEXT NOT NULL DEFAULT '',
  sent_seq INTEGER NOT NULL DEFAULT 0,  -- our history they've confirmed
  last_sync INTEGER NOT NULL DEFAULT 0  -- ms, when we last finished a sync with them
);
CREATE TABLE conflicts (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  tbl TEXT NOT NULL, id TEXT NOT NULL,
  kept_at INTEGER NOT NULL, kept_device TEXT NOT NULL,
  lost_at INTEGER NOT NULL, lost_device TEXT NOT NULL,
  resolved INTEGER NOT NULL DEFAULT 0
);
"#,
];

pub struct Store {
    conn: Connection,
    device: String,
}

/// One version of one record, as stored in history and sent between peers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub table: String,
    pub id: String,
    pub updated_at: i64,
    pub device: String,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub device: String,
    /// The peer's iroh endpoint id.
    pub node: String,
    pub name: String,
    pub sent_seq: i64,
    pub last_sync: i64,
}

/// The same record edited on two devices between syncs. Newest won; the
/// loser is still in history for a future resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub seq: i64,
    pub table: String,
    pub id: String,
    pub kept: (i64, String),
    pub lost: (i64, String),
    pub resolved: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    /// History entries we didn't have yet.
    pub new_entries: usize,
    /// Live records that changed.
    pub changed: usize,
    pub conflicts: usize,
    /// Entries we couldn't decode (kept in history, not applied).
    pub unreadable: usize,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Store> {
        Store::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Store> {
        Store::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        // FULL: a logged expense survives a power cut, at no cost we'd notice.
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;")?;
        let mut store = Store { conn, device: String::new() };
        store.migrate()?;
        store.device = match store.meta("device")? {
            Some(d) => d,
            None => {
                let d = random_id();
                store.set_meta("device", &d)?;
                d
            }
        };
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let done: i64 = self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let tx = self.conn.transaction()?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(done as usize) {
            tx.execute_batch(sql)?;
            tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        }
        tx.commit()
    }

    /// This device's id, made once on first open.
    pub fn device(&self) -> &str {
        &self.device
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        self.conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional()
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)", params![key, value])?;
        Ok(())
    }

    /// Everything, tombstones included (sync needs them; reads drop them).
    pub fn load(&self) -> Result<Book> {
        Ok(Book {
            wallets: self.all("SELECT id, updated_at, deleted, name, color, ord, theme FROM wallets", wallet_from_row)?,
            transactions: self.all(
                "SELECT id, updated_at, deleted, wallet_id, amount, kind, date, note, counts_toward_budget FROM transactions",
                tx_from_row,
            )?,
            budgets: self.all("SELECT id, updated_at, deleted, total, start_date, days, split FROM budgets", budget_from_row)?,
            changes: vec![],
        })
    }

    /// Persists the book's local changes and records them in history.
    pub fn commit(&mut self, book: &mut Book) -> Result<usize> {
        let mut changes = std::mem::take(&mut book.changes);
        let mut seen = std::collections::HashSet::new();
        changes.retain(|c| seen.insert(c.clone()));
        let tx = self.conn.transaction()?;
        let mut n = 0;
        for c in &changes {
            if let Some(rec) = find(book, c) {
                put(&tx, &rec)?;
                tx.execute(
                    "INSERT OR IGNORE INTO history (tbl, id, updated_at, device, via, data) VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
                    params![rec.table(), rec.id(), rec.updated_at(), self.device, rec.to_json()],
                )?;
                n += 1;
            }
        }
        if let Err(e) = tx.commit() {
            book.changes = changes; // keep them so the caller can retry
            return Err(e);
        }
        Ok(n)
    }

    // ---- sync ----

    /// Remembers a paired device. Pairing again updates its node and name.
    pub fn pair(&self, device: &str, node: &str, name: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO peers (device, node, name) VALUES (?1, ?2, ?3)
             ON CONFLICT(device) DO UPDATE SET node = excluded.node, name = excluded.name",
            params![device, node, name],
        )?;
        Ok(())
    }

    pub fn unpair(&self, device: &str) -> Result<()> {
        self.conn.execute("DELETE FROM peers WHERE device = ?1", [device])?;
        Ok(())
    }

    pub fn peers(&self) -> Result<Vec<Peer>> {
        self.all("SELECT device, node, name, sent_seq, last_sync FROM peers ORDER BY name", peer_from_row)
    }

    pub fn peer_by_node(&self, node: &str) -> Result<Option<Peer>> {
        self.conn
            .query_row("SELECT device, node, name, sent_seq, last_sync FROM peers WHERE node = ?1", [node], peer_from_row)
            .optional()
    }

    /// History the peer hasn't confirmed yet, minus what it gave us.
    /// Returns the entries and the seq to confirm once they've landed.
    pub fn outgoing(&self, peer: &str) -> Result<(Vec<HistoryEntry>, i64)> {
        let sent: i64 = self.conn.query_row("SELECT sent_seq FROM peers WHERE device = ?1", [peer], |r| r.get(0))?;
        let mut stmt = self.conn.prepare(
            "SELECT seq, tbl, id, updated_at, device, data FROM history WHERE seq > ?1 AND via != ?2 ORDER BY seq",
        )?;
        let mut max = sent;
        let rows = stmt.query_map(params![sent, peer], |r| {
            Ok((r.get::<_, i64>(0)?, HistoryEntry { table: r.get(1)?, id: r.get(2)?, updated_at: r.get(3)?, device: r.get(4)?, data: r.get(5)? }))
        })?;
        let mut out = vec![];
        for row in rows {
            let (seq, e) = row?;
            max = max.max(seq);
            out.push(e);
        }
        // Entries the peer gave us still move the cursor past them.
        let top: i64 = self.conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM history", [], |r| r.get(0))?;
        Ok((out, max.max(top)))
    }

    /// The peer confirmed everything up to `seq`.
    pub fn mark_sent(&self, peer: &str, seq: i64) -> Result<()> {
        self.conn.execute("UPDATE peers SET sent_seq = MAX(sent_seq, ?2) WHERE device = ?1", params![peer, seq])?;
        Ok(())
    }

    pub fn finish_sync(&self, peer: &str, now: i64) -> Result<()> {
        self.conn.execute("UPDATE peers SET last_sync = ?2 WHERE device = ?1", params![peer, now])?;
        Ok(())
    }

    /// Merges a peer's history into ours and into `book`. Newest wins.
    /// Edits to the same record on both sides since the last sync are logged
    /// as conflicts.
    pub fn apply_remote(&mut self, book: &mut Book, peer: &str, entries: &[HistoryEntry]) -> Result<Applied> {
        let last_sync: i64 = self.conn.query_row("SELECT last_sync FROM peers WHERE device = ?1", [peer], |r| r.get(0))?;
        let me = self.device.clone();
        let tx = self.conn.transaction()?;
        let mut out = Applied::default();
        for e in entries {
            let fresh = tx.execute(
                "INSERT OR IGNORE INTO history (tbl, id, updated_at, device, via, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![e.table, e.id, e.updated_at, e.device, peer, e.data],
            )?;
            if fresh == 0 {
                continue;
            }
            out.new_entries += 1;
            let Some(incoming) = Rec::from_json(&e.table, &e.data) else {
                out.unreadable += 1;
                continue;
            };
            let local = current(book, &incoming);
            let won = match &local {
                None => true,
                Some(l) => (incoming.updated_at(), deleted(&incoming)) > (l.updated_at(), deleted(l)),
            };
            if let Some(l) = local.filter(|l| *l != incoming) {
                let local_device: String = tx
                    .query_row(
                        "SELECT device FROM history WHERE tbl = ?1 AND id = ?2 AND updated_at = ?3 ORDER BY seq LIMIT 1",
                        params![l.table(), l.id(), l.updated_at()],
                        |r| r.get(0),
                    )
                    .optional()?
                    .unwrap_or_else(|| me.clone());
                if local_device != e.device && l.updated_at() > last_sync && incoming.updated_at() > last_sync {
                    let (kept, lost) = if won {
                        ((incoming.updated_at(), e.device.clone()), (l.updated_at(), local_device))
                    } else {
                        ((l.updated_at(), local_device), (incoming.updated_at(), e.device.clone()))
                    };
                    tx.execute(
                        "INSERT INTO conflicts (tbl, id, kept_at, kept_device, lost_at, lost_device) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![e.table, e.id, kept.0, kept.1, lost.0, lost.1],
                    )?;
                    out.conflicts += 1;
                }
            }
            if won {
                put(&tx, &incoming)?;
                replace(book, incoming);
                out.changed += 1;
            }
        }
        tx.commit()?;
        Ok(out)
    }

    pub fn conflicts(&self) -> Result<Vec<Conflict>> {
        self.all("SELECT seq, tbl, id, kept_at, kept_device, lost_at, lost_device, resolved FROM conflicts ORDER BY seq", |r| {
            Ok(Conflict {
                seq: r.get(0)?,
                table: r.get(1)?,
                id: r.get(2)?,
                kept: (r.get(3)?, r.get(4)?),
                lost: (r.get(5)?, r.get(6)?),
                resolved: r.get(7)?,
            })
        })
    }

    /// Every version of one record, oldest first: the audit trail.
    pub fn history_of(&self, table: &str, id: &str) -> Result<Vec<HistoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT tbl, id, updated_at, device, data FROM history WHERE tbl = ?1 AND id = ?2 ORDER BY updated_at, seq",
        )?;
        let rows = stmt.query_map([table, id], |r| {
            Ok(HistoryEntry { table: r.get(0)?, id: r.get(1)?, updated_at: r.get(2)?, device: r.get(3)?, data: r.get(4)? })
        })?;
        rows.collect()
    }

    pub fn setting(&self, key: &str) -> Result<Option<(String, i64)>> {
        self.conn
            .query_row("SELECT value, updated_at FROM settings WHERE key = ?1", [key], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
    }

    /// Last write wins, like records: an older `at` is ignored.
    pub fn set_setting(&self, key: &str, value: &str, at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at
             WHERE excluded.updated_at > settings.updated_at",
            params![key, value, at],
        )?;
        Ok(())
    }

    fn all<T>(&self, sql: &str, f: fn(&Row) -> Result<T>) -> Result<Vec<T>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], f)?;
        rows.collect()
    }
}

fn random_id() -> String {
    let s = RandomState::new();
    format!("{:016x}{:016x}", s.hash_one(1u8), s.hash_one(2u8))
}

fn find(book: &Book, c: &Change) -> Option<Rec> {
    match c {
        Change::Wallet(id) => book.wallets.iter().find(|w| &w.id == id).cloned().map(Rec::Wallet),
        Change::Transaction(id) => book.transactions.iter().find(|t| &t.id == id).cloned().map(Rec::Transaction),
        Change::Budget(id) => book.budgets.iter().find(|b| &b.id == id).cloned().map(Rec::Budget),
    }
}

fn current(book: &Book, like: &Rec) -> Option<Rec> {
    let id = like.id().to_string();
    find(
        book,
        &match like {
            Rec::Wallet(_) => Change::Wallet(id),
            Rec::Transaction(_) => Change::Transaction(id),
            Rec::Budget(_) => Change::Budget(id),
        },
    )
}

fn deleted(r: &Rec) -> bool {
    match r {
        Rec::Wallet(x) => x.deleted,
        Rec::Transaction(x) => x.deleted,
        Rec::Budget(x) => x.deleted,
    }
}

fn replace(book: &mut Book, r: Rec) {
    fn set<T: Clone>(v: &mut Vec<T>, new: T, same: impl Fn(&T) -> bool) {
        match v.iter_mut().find(|x| same(x)) {
            Some(slot) => *slot = new,
            None => v.push(new),
        }
    }
    match r {
        Rec::Wallet(w) => set(&mut book.wallets, w.clone(), |x| x.id == w.id),
        Rec::Transaction(t) => set(&mut book.transactions, t.clone(), |x| x.id == t.id),
        Rec::Budget(b) => set(&mut book.budgets, b.clone(), |x| x.id == b.id),
    }
}

fn put(tx: &Tx, rec: &Rec) -> Result<usize> {
    match rec {
        Rec::Wallet(w) => tx.execute(
            "INSERT OR REPLACE INTO wallets (id, updated_at, deleted, name, color, ord, theme) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![w.id, w.updated_at, w.deleted, w.name, w.color, w.order, w.theme.as_ref().map(theme_to_json)],
        ),
        Rec::Transaction(t) => tx.execute(
            "INSERT OR REPLACE INTO transactions (id, updated_at, deleted, wallet_id, amount, kind, date, note, counts_toward_budget)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![t.id, t.updated_at, t.deleted, t.wallet_id, t.amount, kind_str(t.kind), t.date.to_string(), t.note, t.counts_toward_budget],
        ),
        Rec::Budget(b) => {
            let split = serde_json::to_string(&b.split).expect("a map of strings to numbers always serializes");
            tx.execute(
                "INSERT OR REPLACE INTO budgets (id, updated_at, deleted, total, start_date, days, split) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![b.id, b.updated_at, b.deleted, b.total, b.start_date.to_string(), b.days, split],
            )
        }
    }
}

fn peer_from_row(r: &Row) -> Result<Peer> {
    Ok(Peer { device: r.get(0)?, node: r.get(1)?, name: r.get(2)?, sent_seq: r.get(3)?, last_sync: r.get(4)? })
}

fn wallet_from_row(r: &Row) -> Result<Wallet> {
    let theme: Option<String> = r.get(6)?;
    Ok(Wallet {
        id: r.get(0)?,
        updated_at: r.get(1)?,
        deleted: r.get(2)?,
        name: r.get(3)?,
        color: r.get(4)?,
        order: r.get(5)?,
        theme: theme.map(|s| theme_from_json(&s)).transpose().map_err(|e| bad(6, e))?,
    })
}

fn tx_from_row(r: &Row) -> Result<Transaction> {
    let kind: String = r.get(5)?;
    Ok(Transaction {
        id: r.get(0)?,
        updated_at: r.get(1)?,
        deleted: r.get(2)?,
        wallet_id: r.get(3)?,
        amount: r.get(4)?,
        kind: kind_from(&kind).ok_or_else(|| bad(5, format!("unknown kind {kind}")))?,
        date: date_col(r, 6)?,
        note: r.get(7)?,
        counts_toward_budget: r.get(8)?,
    })
}

fn budget_from_row(r: &Row) -> Result<Budget> {
    let split: String = r.get(6)?;
    Ok(Budget {
        id: r.get(0)?,
        updated_at: r.get(1)?,
        deleted: r.get(2)?,
        total: r.get(3)?,
        start_date: date_col(r, 4)?,
        days: r.get(5)?,
        split: serde_json::from_str::<BTreeMap<String, u32>>(&split).map_err(|e| bad(6, e.to_string()))?,
    })
}

fn date_col(r: &Row, i: usize) -> Result<Date> {
    let s: String = r.get(i)?;
    Date::parse(&s).ok_or_else(|| bad(i, format!("bad date {s}")))
}

fn theme_to_json(t: &Theme) -> String {
    let mut m = Map::new();
    for (k, v) in [("bg", &t.bg), ("ink", &t.ink), ("accent", &t.accent)] {
        if let Some(v) = v {
            m.insert(k.into(), Value::String(v.clone()));
        }
    }
    Value::Object(m).to_string()
}

fn theme_from_json(s: &str) -> std::result::Result<Theme, String> {
    let v: Value = serde_json::from_str(s).map_err(|e| e.to_string())?;
    let get = |k: &str| v.get(k).and_then(Value::as_str).map(String::from);
    Ok(Theme { bg: get("bg"), ink: get("ink"), accent: get("accent") })
}

/// A row that doesn't fit the model: surfaced as a conversion error.
fn bad(col: usize, msg: String) -> Error {
    Error::FromSqlConversionFailure(col, rusqlite::types::Type::Text, msg.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wallet_core::Env;
    use wallet_core::budget::even_split;

    struct FakeEnv {
        now: i64,
        next: u32,
        prefix: &'static str,
    }

    impl Env for FakeEnv {
        fn now(&self) -> i64 {
            self.now
        }
        fn today(&self) -> Date {
            Date::parse("2026-10-29").unwrap()
        }
        fn new_id(&mut self) -> String {
            self.next += 1;
            format!("{}{}", self.prefix, self.next)
        }
    }

    fn env() -> FakeEnv {
        FakeEnv { now: 1_000, next: 0, prefix: "id" }
    }

    fn sample(env: &mut FakeEnv) -> Book {
        let mut book = Book::default();
        let cash = book.create_wallet(env, "Cash", "#ffffff", 50_000);
        let bank = book.create_wallet(env, "Bank", "#ffffff", 100_000);
        book.set_wallet_theme(env, &cash, Theme { bg: Some("#000000".into()), ink: Some("auto".into()), accent: None });
        let split = even_split(&[cash.clone(), bank]);
        book.save_budget(env, Budget { id: String::new(), updated_at: 0, deleted: false, total: 14_000, start_date: env.today(), days: 14, split });
        book.log_expense(env, &cash, 2_500, "lunch", None, None);
        book
    }

    fn same(a: &Book, b: &Book) {
        let sort = |mut v: Vec<String>| {
            v.sort();
            v
        };
        assert_eq!(sort(a.wallets.iter().map(|w| format!("{w:?}")).collect()), sort(b.wallets.iter().map(|w| format!("{w:?}")).collect()));
        assert_eq!(sort(a.transactions.iter().map(|t| format!("{t:?}")).collect()), sort(b.transactions.iter().map(|t| format!("{t:?}")).collect()));
        assert_eq!(sort(a.budgets.iter().map(|x| format!("{x:?}")).collect()), sort(b.budgets.iter().map(|x| format!("{x:?}")).collect()));
    }

    #[test]
    fn round_trip() {
        let mut env = env();
        let mut book = sample(&mut env);
        let mut store = Store::open_in_memory().unwrap();
        store.commit(&mut book).unwrap();
        assert!(book.changes.is_empty());
        same(&book, &store.load().unwrap());
    }

    #[test]
    fn updates_and_deletes_replace_rows() {
        let mut env = env();
        let mut book = sample(&mut env);
        let mut store = Store::open_in_memory().unwrap();
        store.commit(&mut book).unwrap();

        env.now = 2_000;
        let cash = book.wallets[0].id.clone();
        let lunch = book.transactions.last().unwrap().id.clone();
        book.rename_wallet(&env, &cash, "Pocket");
        book.delete_transaction(&env, &lunch);
        book.set_wallet_theme(&env, &cash, Theme::default());
        store.commit(&mut book).unwrap();

        let loaded = store.load().unwrap();
        same(&book, &loaded);
        assert_eq!(loaded.balance(&cash), 50_000);
        assert_eq!(loaded.wallet(&cash).unwrap().theme, None);
        // Every version kept, stamped with this device.
        let trail = store.history_of("transactions", &lunch).unwrap();
        assert_eq!(trail.len(), 2);
        assert!(trail.iter().all(|e| e.device == store.device()));
    }

    #[test]
    fn codec_round_trips_every_record() {
        let mut env = env();
        let book = sample(&mut env);
        for r in book.wallets.iter().cloned().map(Rec::Wallet)
            .chain(book.transactions.iter().cloned().map(Rec::Transaction))
            .chain(book.budgets.iter().cloned().map(Rec::Budget))
        {
            assert_eq!(Rec::from_json(r.table(), &r.to_json()), Some(r));
        }
    }

    #[test]
    fn settings_last_write_wins() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.setting("currency").unwrap(), None);
        store.set_setting("currency", "PHP", 10).unwrap();
        store.set_setting("currency", "USD", 5).unwrap();
        assert_eq!(store.setting("currency").unwrap(), Some(("PHP".into(), 10)));
        store.set_setting("currency", "EUR", 11).unwrap();
        assert_eq!(store.setting("currency").unwrap().unwrap().0, "EUR");
    }

    #[test]
    fn file_reopens_with_same_device() {
        let path = std::env::temp_dir().join(format!("wallet-store-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut env = env();
        let mut book = sample(&mut env);
        let device = {
            let mut store = Store::open(&path).unwrap();
            store.commit(&mut book).unwrap();
            store.device().to_string()
        };
        let store = Store::open(&path).unwrap();
        assert_eq!(store.device(), device);
        same(&book, &store.load().unwrap());
        drop(store);
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
        }
    }

    /// Two devices, in memory, synced by hand (what the network layer does).
    struct Device {
        store: Store,
        book: Book,
    }

    fn device() -> Device {
        Device { store: Store::open_in_memory().unwrap(), book: Book::default() }
    }

    fn pair(a: &Device, b: &Device) {
        a.store.pair(b.store.device(), &format!("node-{}", b.store.device()), "b").unwrap();
        b.store.pair(a.store.device(), &format!("node-{}", a.store.device()), "a").unwrap();
    }

    fn sync(a: &mut Device, b: &mut Device, now: i64) -> (Applied, Applied) {
        let (ida, idb) = (a.store.device().to_string(), b.store.device().to_string());
        let (to_b, a_top) = a.store.outgoing(&idb).unwrap();
        let (to_a, b_top) = b.store.outgoing(&ida).unwrap();
        let got_b = b.store.apply_remote(&mut b.book, &ida, &to_b).unwrap();
        let got_a = a.store.apply_remote(&mut a.book, &idb, &to_a).unwrap();
        a.store.mark_sent(&idb, a_top).unwrap();
        b.store.mark_sent(&ida, b_top).unwrap();
        a.store.finish_sync(&idb, now).unwrap();
        b.store.finish_sync(&ida, now).unwrap();
        (got_a, got_b)
    }

    #[test]
    fn separate_transactions_both_survive() {
        let (mut laptop, mut phone) = (device(), device());
        pair(&laptop, &phone);
        let mut e = env();
        let cash = laptop.book.create_wallet(&mut e, "Cash", "#fff", 10_000);
        laptop.store.commit(&mut laptop.book).unwrap();
        sync(&mut laptop, &mut phone, 1_500);
        assert_eq!(phone.book.balance(&cash), 10_000);

        // −20 on the laptop, −200 on the phone: two records, both kept.
        let mut el = FakeEnv { now: 2_000, next: 0, prefix: "lap" };
        let mut ep = FakeEnv { now: 2_100, next: 0, prefix: "pho" };
        laptop.book.log_expense(&mut el, &cash, 2_000, "", None, None);
        phone.book.log_expense(&mut ep, &cash, 20_000, "", None, None);
        laptop.store.commit(&mut laptop.book).unwrap();
        phone.store.commit(&mut phone.book).unwrap();
        let (a, b) = sync(&mut laptop, &mut phone, 3_000);
        assert_eq!((a.conflicts, b.conflicts), (0, 0));
        assert_eq!(laptop.book.balance(&cash), -12_000);
        assert_eq!(phone.book.balance(&cash), -12_000);
        same(&laptop.book, &phone.book);
        same(&laptop.store.load().unwrap(), &phone.store.load().unwrap());

        // Nothing new: nothing sent.
        let (a, b) = sync(&mut laptop, &mut phone, 4_000);
        assert_eq!((a.new_entries, b.new_entries), (0, 0));
    }

    #[test]
    fn same_record_edited_on_both_is_a_conflict_newest_wins() {
        let (mut laptop, mut phone) = (device(), device());
        pair(&laptop, &phone);
        let mut e = env();
        let cash = laptop.book.create_wallet(&mut e, "Cash", "#fff", 0);
        laptop.store.commit(&mut laptop.book).unwrap();
        sync(&mut laptop, &mut phone, 1_500);

        laptop.book.rename_wallet(&FakeEnv { now: 2_000, ..env() }, &cash, "Pocket");
        phone.book.rename_wallet(&FakeEnv { now: 2_500, ..env() }, &cash, "Wallet");
        laptop.store.commit(&mut laptop.book).unwrap();
        phone.store.commit(&mut phone.book).unwrap();
        sync(&mut laptop, &mut phone, 3_000);

        assert_eq!(laptop.book.wallet(&cash).unwrap().name, "Wallet");
        assert_eq!(phone.book.wallet(&cash).unwrap().name, "Wallet");
        let c = laptop.store.conflicts().unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!((c[0].kept.0, c[0].lost.0), (2_500, 2_000));
        assert_eq!(c[0].lost.1, laptop.store.device());
        assert_eq!(phone.store.conflicts().unwrap().len(), 1);
        // Both versions in both histories, for audit and a future resolver.
        assert_eq!(laptop.store.history_of("wallets", &cash).unwrap().len(), 3);
        assert_eq!(phone.store.history_of("wallets", &cash).unwrap().len(), 3);
    }

    #[test]
    fn history_relays_through_a_third_device() {
        let (mut phone, mut laptop, mut daemon) = (device(), device(), device());
        pair(&phone, &daemon);
        pair(&laptop, &daemon);
        let mut e = env();
        let cash = phone.book.create_wallet(&mut e, "Cash", "#fff", 5_000);
        phone.store.commit(&mut phone.book).unwrap();
        sync(&mut phone, &mut daemon, 1_500);
        sync(&mut laptop, &mut daemon, 1_600);
        assert_eq!(laptop.book.balance(&cash), 5_000);
        let trail = laptop.store.history_of("wallets", &cash).unwrap();
        assert_eq!(trail[0].device, phone.store.device()); // made on the phone, not the daemon
    }
}
