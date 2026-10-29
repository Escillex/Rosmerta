//! SQLite storage for a `wallet_core::Book`. Load everything at start, then
//! `commit` after each write to persist what the book says changed.
//! Plain columns and indexes, so later analytics can run SQL straight on it.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row, Transaction as Tx, params};
use serde_json::{Map, Value};
use wallet_core::{Book, Budget, Change, Date, Kind, Theme, Transaction, Wallet};

pub use rusqlite::Error;
pub type Result<T> = rusqlite::Result<T>;

/// Each entry runs once, in order; `PRAGMA user_version` counts how many ran.
/// Never edit a shipped entry, only append.
const MIGRATIONS: &[&str] = &[r#"
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
"#];

pub struct Store {
    conn: Connection,
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
        let mut store = Store { conn };
        store.migrate()?;
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

    /// Persists the book's local changes and queues them for sync.
    pub fn commit(&mut self, book: &mut Book) -> Result<usize> {
        self.write(book, true)
    }

    /// Persists changes that came from the server; they don't go back out.
    pub fn commit_remote(&mut self, book: &mut Book) -> Result<usize> {
        self.write(book, false)
    }

    fn write(&mut self, book: &mut Book, queue: bool) -> Result<usize> {
        let mut changes = std::mem::take(&mut book.changes);
        let mut seen = std::collections::HashSet::new();
        changes.retain(|c| seen.insert(c.clone()));
        let tx = self.conn.transaction()?;
        let mut n = 0;
        for c in &changes {
            let (tbl, id) = match c {
                Change::Wallet(id) => ("wallets", id),
                Change::Transaction(id) => ("transactions", id),
                Change::Budget(id) => ("budgets", id),
            };
            let written = match c {
                Change::Wallet(_) => book.wallets.iter().find(|w| &w.id == id).map(|w| put_wallet(&tx, w)),
                Change::Transaction(_) => book.transactions.iter().find(|t| &t.id == id).map(|t| put_tx(&tx, t)),
                Change::Budget(_) => book.budgets.iter().find(|b| &b.id == id).map(|b| put_budget(&tx, b)),
            };
            if let Some(res) = written {
                res?;
                n += 1;
                if queue {
                    tx.execute("INSERT INTO outbox (tbl, id) VALUES (?1, ?2)", params![tbl, id])?;
                }
            }
        }
        if let Err(e) = tx.commit() {
            book.changes = changes; // keep them so the caller can retry
            return Err(e);
        }
        Ok(n)
    }

    /// Queued local changes, oldest first: `(seq, change)`.
    pub fn outbox(&self) -> Result<Vec<(i64, Change)>> {
        let mut stmt = self.conn.prepare("SELECT seq, tbl, id FROM outbox ORDER BY seq")?;
        let rows = stmt.query_map([], |r| {
            let (seq, tbl, id): (i64, String, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
            let change = match tbl.as_str() {
                "wallets" => Change::Wallet(id),
                "transactions" => Change::Transaction(id),
                "budgets" => Change::Budget(id),
                other => return Err(bad(1, format!("unknown table {other}"))),
            };
            Ok((seq, change))
        })?;
        rows.collect()
    }

    /// Drops outbox entries the server acknowledged, up to and including `seq`.
    pub fn ack(&self, seq: i64) -> Result<()> {
        self.conn.execute("DELETE FROM outbox WHERE seq <= ?1", [seq])?;
        Ok(())
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

fn put_wallet(tx: &Tx, w: &Wallet) -> Result<usize> {
    tx.execute(
        "INSERT OR REPLACE INTO wallets (id, updated_at, deleted, name, color, ord, theme) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![w.id, w.updated_at, w.deleted, w.name, w.color, w.order, w.theme.as_ref().map(theme_to_json)],
    )
}

fn put_tx(tx: &Tx, t: &Transaction) -> Result<usize> {
    tx.execute(
        "INSERT OR REPLACE INTO transactions (id, updated_at, deleted, wallet_id, amount, kind, date, note, counts_toward_budget)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![t.id, t.updated_at, t.deleted, t.wallet_id, t.amount, kind_str(t.kind), t.date.to_string(), t.note, t.counts_toward_budget],
    )
}

fn put_budget(tx: &Tx, b: &Budget) -> Result<usize> {
    let split = serde_json::to_string(&b.split).expect("a map of strings to numbers always serializes");
    tx.execute(
        "INSERT OR REPLACE INTO budgets (id, updated_at, deleted, total, start_date, days, split) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![b.id, b.updated_at, b.deleted, b.total, b.start_date.to_string(), b.days, split],
    )
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
        kind: match kind.as_str() {
            "expense" => Kind::Expense,
            "income" => Kind::Income,
            "adjustment" => Kind::Adjustment,
            other => return Err(bad(5, format!("unknown kind {other}"))),
        },
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

fn kind_str(k: Kind) -> &'static str {
    match k {
        Kind::Expense => "expense",
        Kind::Income => "income",
        Kind::Adjustment => "adjustment",
    }
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
            format!("id{}", self.next)
        }
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
        let mut env = FakeEnv { now: 1_000, next: 0 };
        let mut book = sample(&mut env);
        let mut store = Store::open_in_memory().unwrap();
        store.commit(&mut book).unwrap();
        assert!(book.changes.is_empty());
        same(&book, &store.load().unwrap());
    }

    #[test]
    fn updates_and_deletes_replace_rows() {
        let mut env = FakeEnv { now: 1_000, next: 0 };
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
    }

    #[test]
    fn outbox_queues_local_changes_only() {
        let mut env = FakeEnv { now: 1_000, next: 0 };
        let mut book = sample(&mut env);
        let mut store = Store::open_in_memory().unwrap();
        let n = store.commit(&mut book).unwrap();
        let out = store.outbox().unwrap();
        assert_eq!(out.len(), n);
        assert!(matches!(out[0].1, Change::Wallet(_)));

        store.ack(out[2].0).unwrap();
        assert_eq!(store.outbox().unwrap().len(), n - 3);

        let mut remote = Book::default();
        remote.wallets.push(Wallet { id: "r".into(), updated_at: 9, deleted: false, name: "Remote".into(), color: "#fff".into(), order: 5, theme: None });
        wallet_core::sync::merge_book(&mut book, &remote);
        store.commit_remote(&mut book).unwrap();
        assert_eq!(store.outbox().unwrap().len(), n - 3);
        assert!(store.load().unwrap().wallet("r").is_some());
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
    fn file_reopens_without_rerunning_migrations() {
        let path = std::env::temp_dir().join(format!("wallet-store-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut env = FakeEnv { now: 1_000, next: 0 };
        let mut book = sample(&mut env);
        {
            let mut store = Store::open(&path).unwrap();
            store.commit(&mut book).unwrap();
        }
        let store = Store::open(&path).unwrap();
        same(&book, &store.load().unwrap());
        drop(store);
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
        }
    }
}
