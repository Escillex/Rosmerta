//! Records as JSON, for the history table and the sync wire format.
//! Same camelCase shape as the playground's snapshot.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use wallet_core::{Budget, Date, Kind, Theme, Transaction, Wallet};

/// One record of any table.
#[derive(Debug, Clone, PartialEq)]
pub enum Rec {
    Wallet(Wallet),
    Transaction(Transaction),
    Budget(Budget),
}

impl Rec {
    pub fn table(&self) -> &'static str {
        match self {
            Rec::Wallet(_) => "wallets",
            Rec::Transaction(_) => "transactions",
            Rec::Budget(_) => "budgets",
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Rec::Wallet(r) => &r.id,
            Rec::Transaction(r) => &r.id,
            Rec::Budget(r) => &r.id,
        }
    }

    pub fn updated_at(&self) -> i64 {
        match self {
            Rec::Wallet(r) => r.updated_at,
            Rec::Transaction(r) => r.updated_at,
            Rec::Budget(r) => r.updated_at,
        }
    }

    pub fn to_json(&self) -> String {
        let v = match self {
            Rec::Wallet(w) => {
                let mut v = json!({"id": w.id, "updatedAt": w.updated_at, "deleted": w.deleted,
                    "name": w.name, "color": w.color, "order": w.order});
                if let Some(t) = &w.theme {
                    let mut m = Map::new();
                    for (k, c) in [("bg", &t.bg), ("ink", &t.ink), ("accent", &t.accent)] {
                        if let Some(c) = c {
                            m.insert(k.into(), Value::String(c.clone()));
                        }
                    }
                    v["theme"] = Value::Object(m);
                }
                v
            }
            Rec::Transaction(t) => json!({"id": t.id, "updatedAt": t.updated_at, "deleted": t.deleted,
                "walletId": t.wallet_id, "amount": t.amount, "kind": kind_str(t.kind), "date": t.date.to_string(),
                "note": t.note, "countsTowardBudget": t.counts_toward_budget}),
            Rec::Budget(b) => json!({"id": b.id, "updatedAt": b.updated_at, "deleted": b.deleted,
                "total": b.total, "startDate": b.start_date.to_string(), "days": b.days, "split": b.split}),
        };
        v.to_string()
    }

    pub fn from_json(table: &str, s: &str) -> Option<Rec> {
        let v: Value = serde_json::from_str(s).ok()?;
        let st = |k: &str| v.get(k).and_then(Value::as_str).map(String::from);
        let int = |k: &str| v.get(k).and_then(Value::as_i64);
        let (id, updated_at, deleted) = (st("id")?, int("updatedAt")?, v.get("deleted")?.as_bool()?);
        Some(match table {
            "wallets" => Rec::Wallet(Wallet {
                id,
                updated_at,
                deleted,
                name: st("name")?,
                color: st("color")?,
                order: int("order")?,
                theme: v.get("theme").map(|t| {
                    let c = |k: &str| t.get(k).and_then(Value::as_str).map(String::from);
                    Theme { bg: c("bg"), ink: c("ink"), accent: c("accent") }
                }),
            }),
            "transactions" => Rec::Transaction(Transaction {
                id,
                updated_at,
                deleted,
                wallet_id: st("walletId")?,
                amount: int("amount")?,
                kind: kind_from(&st("kind")?)?,
                date: Date::parse(&st("date")?)?,
                note: st("note")?,
                counts_toward_budget: v.get("countsTowardBudget")?.as_bool()?,
            }),
            "budgets" => Rec::Budget(Budget {
                id,
                updated_at,
                deleted,
                total: int("total")?,
                start_date: Date::parse(&st("startDate")?)?,
                days: int("days")?,
                split: serde_json::from_value::<BTreeMap<String, u32>>(v.get("split")?.clone()).ok()?,
            }),
            _ => return None,
        })
    }
}

pub fn kind_str(k: Kind) -> &'static str {
    match k {
        Kind::Expense => "expense",
        Kind::Income => "income",
        Kind::Adjustment => "adjustment",
    }
}

pub fn kind_from(s: &str) -> Option<Kind> {
    match s {
        "expense" => Some(Kind::Expense),
        "income" => Some(Kind::Income),
        "adjustment" => Some(Kind::Adjustment),
        _ => None,
    }
}
