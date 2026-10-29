//! Record types. Every record carries `id`, `updated_at` and `deleted`, which
//! last-write-wins sync relies on. Balances are never stored.

use std::collections::BTreeMap;

use crate::date::Date;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Theme {
    pub bg: Option<String>,
    /// `Some("auto")` means pick black or white for contrast.
    pub ink: Option<String>,
    pub accent: Option<String>,
}

impl Theme {
    pub fn is_empty(&self) -> bool {
        self.bg.is_none() && self.ink.is_none() && self.accent.is_none()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Wallet {
    pub id: String,
    pub updated_at: i64,
    pub deleted: bool,
    pub name: String,
    pub color: String,
    pub order: i64,
    pub theme: Option<Theme>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Expense,
    Income,
    /// Starting balances and balance corrections.
    Adjustment,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Transaction {
    pub id: String,
    pub updated_at: i64,
    pub deleted: bool,
    pub wallet_id: String,
    /// Hundredths; negative = money out.
    pub amount: i64,
    pub kind: Kind,
    pub date: Date,
    pub note: String,
    /// Only expenses can be `true`.
    pub counts_toward_budget: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Budget {
    pub id: String,
    pub updated_at: i64,
    pub deleted: bool,
    /// Hundredths, for the whole period.
    pub total: i64,
    pub start_date: Date,
    pub days: i64,
    /// walletId → percent. Adds up to 100; 0 or missing = outside the budget.
    pub split: BTreeMap<String, u32>,
}

impl Budget {
    pub fn end_date(&self) -> Date {
        self.start_date.add_days(self.days - 1)
    }

    pub fn covers(&self, day: Date) -> bool {
        day >= self.start_date && day <= self.end_date()
    }

    pub fn share_of(&self, wallet_id: &str) -> u32 {
        self.split.get(wallet_id).copied().unwrap_or(0)
    }
}

/// Shared sync fields, so merge works over any record type.
pub trait Record {
    fn id(&self) -> &str;
    fn updated_at(&self) -> i64;
    fn deleted(&self) -> bool;
}

macro_rules! record {
    ($t:ty) => {
        impl Record for $t {
            fn id(&self) -> &str {
                &self.id
            }
            fn updated_at(&self) -> i64 {
                self.updated_at
            }
            fn deleted(&self) -> bool {
                self.deleted
            }
        }
    };
}

record!(Wallet);
record!(Transaction);
record!(Budget);
