//! Reads the playground's `.escb` backups (and bare snapshot JSON) into a
//! `Book`. Read-only by design: the native app never writes `.escb`.
//!
//! ```text
//! bytes  0-3   "ESCB"
//! byte   4     version (1)
//! byte   5     key method (1 = PBKDF2-SHA256)
//! bytes  6-9   rounds, big-endian
//! bytes 10-25  salt
//! bytes 26-37  IV
//! bytes 38-…   AES-256-GCM( gzip( JSON snapshot ) ), header as additional data
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use serde_json::Value;
use sha2::Sha256;
use wallet_core::{Book, Budget, Date, Kind, Theme, Transaction, Wallet};

const MAGIC: &[u8; 4] = b"ESCB";
const VERSION: u8 = 1;
const KDF_PBKDF2_SHA256: u8 = 1;
const HEADER_LEN: usize = 38;

#[derive(Debug)]
pub enum ImportError {
    /// "This file isn't a Tools backup."
    NotABackup,
    /// Made by a newer playground version.
    NewerVersion,
    /// AES-GCM can't tell these apart; a wrong password is far more likely.
    WrongPasswordOrDamaged,
    /// Decrypted, but the contents aren't a snapshot we understand.
    BadContents(String),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ImportError::NotABackup => write!(f, "This file isn't a Tools backup."),
            ImportError::NewerVersion => write!(f, "This backup was made by a newer version of the app."),
            ImportError::WrongPasswordOrDamaged => write!(f, "Wrong password, or the file is damaged."),
            ImportError::BadContents(why) => write!(f, "The backup opened but couldn't be read: {why}"),
        }
    }
}

impl std::error::Error for ImportError {}

#[derive(Debug, Clone)]
pub struct Import {
    /// Every record, tombstones included, ready for `sync::merge_book`.
    pub book: Book,
    /// `(code, at)`; `None` when the snapshot predates currencies (treat as PHP).
    pub currency: Option<(String, i64)>,
    /// Records dropped for missing `id`, `updatedAt` or `deleted`, or bad fields.
    pub skipped: usize,
}

/// Cheap signature check, before asking for a password.
pub fn looks_like_backup(bytes: &[u8]) -> bool {
    bytes.len() > HEADER_LEN && bytes.starts_with(MAGIC)
}

/// Decrypts and un-gzips a `.escb` file to its snapshot JSON.
pub fn decrypt(bytes: &[u8], password: &str) -> Result<Vec<u8>, ImportError> {
    if !looks_like_backup(bytes) {
        return Err(ImportError::NotABackup);
    }
    if bytes[4] != VERSION || bytes[5] != KDF_PBKDF2_SHA256 {
        return Err(ImportError::NewerVersion);
    }
    let header = &bytes[..HEADER_LEN];
    let rounds = u32::from_be_bytes(header[6..10].try_into().unwrap());
    let salt = &header[10..26];
    let iv = &header[26..38];

    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, rounds, &mut key);
    let cipher = Aes256Gcm::new_from_slice(&key).expect("32-byte key");
    let gz = cipher
        .decrypt(&Nonce::try_from(iv).expect("12-byte IV"), Payload { msg: &bytes[HEADER_LEN..], aad: header })
        .map_err(|_| ImportError::WrongPasswordOrDamaged)?;

    let mut json = Vec::new();
    flate2::read::GzDecoder::new(&gz[..])
        .read_to_end(&mut json)
        .map_err(|e| ImportError::BadContents(format!("gzip: {e}")))?;
    Ok(json)
}

pub fn import_escb(bytes: &[u8], password: &str) -> Result<Import, ImportError> {
    parse_snapshot(&decrypt(bytes, password)?)
}

/// Reads `tools.finance` and `settings.currency`; everything else is ignored.
pub fn parse_snapshot(json: &[u8]) -> Result<Import, ImportError> {
    let root: Value = serde_json::from_slice(json).map_err(|e| ImportError::BadContents(format!("json: {e}")))?;
    if root.get("format").and_then(Value::as_i64) != Some(1) {
        return Err(ImportError::BadContents("unknown snapshot format".into()));
    }
    let finance = root.pointer("/tools/finance");
    let list = |name: &str| finance.and_then(|f| f.get(name)).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);

    let mut out = Import { book: Book::default(), currency: None, skipped: 0 };
    collect(list("wallets"), wallet, &mut out.book.wallets, &mut out.skipped);
    collect(list("transactions"), transaction, &mut out.book.transactions, &mut out.skipped);
    collect(list("budgets"), budget, &mut out.book.budgets, &mut out.skipped);

    out.currency = root.pointer("/settings/currency").and_then(|c| {
        Some((c.get("value")?.as_str()?.to_string(), c.get("at")?.as_i64()?))
    });
    Ok(out)
}

fn collect<T>(items: &[Value], parse: fn(&Value, Sync) -> Option<T>, into: &mut Vec<T>, skipped: &mut usize) {
    for v in items {
        match sync_fields(v).and_then(|s| parse(v, s)) {
            Some(r) => into.push(r),
            None => *skipped += 1,
        }
    }
}

/// The fields every record must carry.
struct Sync {
    id: String,
    updated_at: i64,
    deleted: bool,
}

fn sync_fields(v: &Value) -> Option<Sync> {
    Some(Sync { id: str_of(v, "id")?, updated_at: int_of(v, "updatedAt")?, deleted: v.get("deleted")?.as_bool()? })
}

fn str_of(v: &Value, k: &str) -> Option<String> {
    v.get(k)?.as_str().map(String::from)
}

/// JSON numbers from JS may come through as floats (e.g. `1250.0`).
fn int_of(v: &Value, k: &str) -> Option<i64> {
    let n = v.get(k)?;
    n.as_i64().or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
}

fn wallet(v: &Value, s: Sync) -> Option<Wallet> {
    let theme = v.get("theme").filter(|t| t.is_object()).map(|t| Theme {
        bg: str_of(t, "bg").filter(|c| !c.is_empty()),
        ink: str_of(t, "ink").filter(|c| !c.is_empty()),
        accent: str_of(t, "accent").filter(|c| !c.is_empty()),
    });
    Some(Wallet {
        id: s.id,
        updated_at: s.updated_at,
        deleted: s.deleted,
        name: str_of(v, "name")?,
        color: str_of(v, "color").unwrap_or_else(|| "#ffffff".into()),
        order: int_of(v, "order").unwrap_or(0),
        theme: theme.filter(|t| !t.is_empty()),
    })
}

fn transaction(v: &Value, s: Sync) -> Option<Transaction> {
    let kind = match v.get("kind")?.as_str()? {
        "expense" => Kind::Expense,
        "income" => Kind::Income,
        "adjustment" => Kind::Adjustment,
        _ => return None,
    };
    Some(Transaction {
        id: s.id,
        updated_at: s.updated_at,
        deleted: s.deleted,
        wallet_id: str_of(v, "walletId")?,
        amount: int_of(v, "amount")?,
        kind,
        date: Date::parse(v.get("date")?.as_str()?)?,
        note: str_of(v, "note").unwrap_or_default(),
        counts_toward_budget: kind == Kind::Expense && v.get("countsTowardBudget").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn budget(v: &Value, s: Sync) -> Option<Budget> {
    let mut split = BTreeMap::new();
    for (k, p) in v.get("split")?.as_object()? {
        let p = p.as_f64()?;
        split.insert(k.clone(), p.round().max(0.0) as u32);
    }
    Some(Budget {
        id: s.id,
        updated_at: s.updated_at,
        deleted: s.deleted,
        total: int_of(v, "total")?,
        start_date: Date::parse(v.get("startDate")?.as_str()?)?,
        days: int_of(v, "days")?,
        split,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SNAPSHOT: &str = r##"{
      "format": 1, "deviceId": "dev", "at": 1730000000000,
      "tools": {
        "finance": {
          "wallets": [
            {"id": "w1", "updatedAt": 1, "deleted": false, "name": "Cash", "color": "#ffffff", "order": 0},
            {"id": "w2", "updatedAt": 2, "deleted": true, "name": "Old", "color": "#ff1a1a", "order": 1,
             "theme": {"bg": "#000000", "ink": "auto", "accent": ""}},
            {"name": "no id", "updatedAt": 3, "deleted": false}
          ],
          "transactions": [
            {"id": "t1", "updatedAt": 5, "deleted": false, "walletId": "w1", "amount": -1250, "kind": "expense",
             "date": "2026-10-28", "note": "lunch", "countsTowardBudget": true},
            {"id": "t2", "updatedAt": 6, "deleted": false, "walletId": "w1", "amount": 50000.0, "kind": "adjustment",
             "date": "2026-10-01", "note": "Starting balance", "countsTowardBudget": true},
            {"id": "t3", "updatedAt": 7, "deleted": false, "walletId": "w1", "amount": 1, "kind": "gift",
             "date": "2026-10-01", "note": ""}
          ],
          "budgets": [
            {"id": "b1", "updatedAt": 8, "deleted": false, "total": 140000, "startDate": "2026-10-20", "days": 14,
             "split": {"w1": 100}}
          ]
        },
        "focus": {"anything": true}
      },
      "settings": {"currency": {"value": "PHP", "at": 9}}
    }"##;

    #[test]
    fn parses_snapshot() {
        let i = parse_snapshot(SNAPSHOT.as_bytes()).unwrap();
        assert_eq!((i.book.wallets.len(), i.book.transactions.len(), i.book.budgets.len()), (2, 2, 1));
        assert_eq!(i.skipped, 2); // wallet without id, unknown kind
        assert_eq!(i.currency, Some(("PHP".into(), 9)));
        assert!(i.book.wallets[1].deleted);
        assert_eq!(i.book.wallets[1].theme.as_ref().unwrap().accent, None);
        let t2 = &i.book.transactions[1];
        assert_eq!((t2.amount, t2.counts_toward_budget), (50_000, false)); // only expenses count
        assert_eq!(i.book.balance("w1"), 48_750);
        assert_eq!(i.book.budgets[0].share_of("w1"), 100);
    }

    #[test]
    fn missing_finance_and_currency_is_fine() {
        let i = parse_snapshot(br#"{"format": 1, "tools": {}}"#).unwrap();
        assert!(i.book.wallets.is_empty());
        assert_eq!(i.currency, None);
    }

    #[test]
    fn rejects_other_files() {
        assert!(matches!(decrypt(b"PK\x03\x04 not a backup at all, just some zip-ish bytes", "x"), Err(ImportError::NotABackup)));
        let mut newer = b"ESCB".to_vec();
        newer.extend([2u8; 40]);
        assert!(matches!(decrypt(&newer, "x"), Err(ImportError::NewerVersion)));
        assert!(matches!(parse_snapshot(br#"{"format": 2}"#), Err(ImportError::BadContents(_))));
    }

    /// The real playground backup, kept out of git. Skipped when absent.
    fn real_backup() -> Option<Vec<u8>> {
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/real.escb")).ok()
    }

    #[test]
    fn real_backup_opens() {
        let Some(bytes) = real_backup() else { return };
        let i = import_escb(&bytes, "test1234").unwrap();
        assert!(!i.book.wallets.is_empty());
        assert_eq!(i.skipped, 0);
    }

    #[test]
    fn real_backup_wrong_password() {
        let Some(bytes) = real_backup() else { return };
        assert!(matches!(decrypt(&bytes, "nope"), Err(ImportError::WrongPasswordOrDamaged)));
    }
}
