# Wallet Native: Project Brief

As of 2026-10-29. Drawn from the playground Wallet in `escillex-tools`.

Wallet becomes a native, Android-first app: a Rust core with SQLite and a Makepad 2.0 UI, rebuilt from the playground version.

---

## 1. Overview

**What exists today.** Wallet is one tool in the SvelteKit playground, served at `/wallet`. It has three screens: today (`/wallet`), calendar (`/wallet/calendar`) and manage (`/wallet/manage`). Data lives on the device in an IndexedDB database called `finance`, accessed through Dexie. The tool is about 2,800 lines.

**What the native app adds.**

- A Rust `wallet-core` crate holding the model, the budget maths, SQLite storage and sync, with no UI code.
- A Makepad 2.0 app on top, with the UI written in Splash.
- Sync to a server you host yourself (a VPS or a home machine), replacing the playground's device-to-device pairing.
- Pots: budgets for spending that isn't daily (bills, savings goals, planned one-offs).

**Scope.** One user, Android first, desktop as a bonus. The playground Wallet keeps running until the native app can import its data.

**Source files.**

| File | What it holds |
| --- | --- |
| `src/lib/finance/db.ts` | Record types and the database schema |
| `src/lib/finance/data.ts` | Every read and write, and the budget maths (~260 lines) |
| `src/lib/finance/Setup.svelte` | First-run setup: wallets, then budget |
| `src/lib/finance/Entry.svelte` | Full-screen amount entry |
| `src/lib/finance/Calendar.svelte` | Month calendar, analytics, plan editor |
| `src/lib/finance/Manage.svelte` | Budget, Wallets and History tabs |
| `src/lib/finance/walletTheme.ts` | Per-wallet colours |
| `src/lib/finance/selection.svelte.ts` | Which wallet is on screen |
| `src/routes/wallet/` | The three screens and their shared layout |
| `src/lib/ui/WalletLoop.svelte`, `detent.svelte.ts` | The endless wallet dial and its momentum maths |
| `src/lib/ui/RollingNumber.svelte`, `ModeSwitch.svelte` | The big rolling number and the mode switch |
| `src/lib/ui/feedback.ts` | Click sound and haptics |
| `src/lib/core/currency.svelte.ts` | Currency setting and formatting |
| `src/lib/core/sync/`, `src/lib/core/backup/` | Sync snapshots and the `.escb` backup file |

---

## 2. Current data model

Three record types hold everything: wallets, transactions and budgets. A balance is never stored; it is always the sum of a wallet's transactions. Schema: `src/lib/finance/db.ts` (Dexie version 1).

### Fields every record carries (`SyncFields`)

Keep these exactly; they make last-write-wins sync work.

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string (UUID v4) | Random, so two devices never collide |
| `updatedAt` | integer, ms since epoch | When it last changed; the newer copy wins on merge |
| `deleted` | boolean | Tombstone, so a delete reaches other devices |

### Wallet (`wallets`, indexed on `order`, `updatedAt`)

| Field | Type | Meaning |
| --- | --- | --- |
| `name` | string | Display name |
| `color` | string (hex) | Swatch colour (setup creates wallets as `#ffffff`) |
| `order` | integer | Position in the list; a new wallet gets the current count |
| `theme` | optional `{ bg?, ink?, accent? }` | The wallet screen's own colours; `ink` may be `'auto'` |

### Transaction (`transactions`, indexed on `walletId`, `date`, `updatedAt`)

| Field | Type | Meaning |
| --- | --- | --- |
| `walletId` | string | The wallet it belongs to |
| `amount` | integer, hundredths | Negative = money out, positive = money in |
| `kind` | `expense` \| `income` \| `adjustment` | Adjustments = starting balances and balance corrections |
| `date` | string `YYYY-MM-DD` | The local day it happened, not a timestamp |
| `note` | string | Free text, may be empty |
| `countsTowardBudget` | boolean | Only expenses can be `true`; income and adjustments are always `false` |

### Budget (`budgets`, indexed on `startDate`, `updatedAt`)

| Field | Type | Meaning |
| --- | --- | --- |
| `total` | integer, hundredths | Amount for the whole period |
| `startDate` | string `YYYY-MM-DD` | First day of the period |
| `days` | integer | Length of the period (setup defaults to 14) |
| `split` | map walletId → percent | Each wallet's share; adds up to 100; 0% or missing = outside the budget |

### Conventions to keep

- Money is always an integer in hundredths of the currency unit (₱12.50 = 1250). Floats only appear in `toCentavos`, which rounds: `Math.round(units * 100)`.
- Currency is a setting, not part of any record. Choices: USD, EUR, GBP, PHP, JPY, KRW, CNY, INR, SGD, AUD, CAD, IDR, MYR, THB, VND, AED. First guess comes from the device region (fallback USD). Changing it only changes display. Data created before currencies existed is treated as PHP. Number style (1,234.56 vs 1.234,56) follows the device language.
- Dates are local calendar days as strings, so a transaction never shifts day with the time zone. `addDays` works on local dates.
- Deletes are soft (`deleted: true`); every read filters tombstones.
- The current budget = the one with the latest `startDate`. Older budgets stay as history.
- Which wallet is on screen (`'all'` or a wallet id) is UI state only, never stored.

---

## 3. Business logic (`data.ts`)

Every write stamps `updatedAt = now` through one helper (`touch`). Every read drops tombstones.

**Balances.** A wallet's balance = sum of `amount` over its live transactions. "All" = sum over every live wallet.

### Budget summary (`budgetSummary`)

Produces the main screen's numbers, per wallet and for "All".

1. Take the current budget. None → no summary → the app shows setup.
2. Period = `startDate` … `startDate + days − 1`. Today's day number is 1-based; it can be < 1 (not started) or > `days` (ended).
3. Spending = live transactions in the period with `kind = expense` and `countsTowardBudget = true`.
4. For each wallet with share > 0%:

```
share     = total × percent / 100
daily     = share / days
allowance = round(daily − spentToday)        (0 outside the period)
remaining = round(share − spentInPeriod)
```

5. Round only at the end. "All" = sum of the per-wallet results.

**Important behaviour:** the daily amount is fixed. Underspending today does not raise tomorrow's allowance (it only shows in `remaining`); overspending makes today negative but does not lower tomorrow. Decide whether the native app keeps this or adds rollover.

### Wallets outside the budget

A wallet at 0% (or missing from `split`) is exempt. Expenses logged from it get `countsTowardBudget = false`. On the allowance view it shows its balance instead, greyed, with "Not in budget · balance".

### Budget cap rule (setup and plan editor)

The budget can't exceed what the in-budget wallets can cover:

```
maxBudget = (current balance of in-budget wallets) + (budget spending already done this period)
```

Setup checks `total ≤ held` (no spending yet). The plan editor uses the formula above. Error text: "Your budget wallets can only cover ₱X. Lower the budget, or put another wallet in it."

Other validation: total > 0, days ≥ 1, start date set, split adds up to exactly 100.

### Every operation

| Operation | Behaviour |
| --- | --- |
| `createWallet(name, color, startingBalance)` | Appends to the order. Non-zero starting balance → `adjustment`, note "Starting balance", outside budget. One DB transaction. |
| `deleteWallet(id)` | Soft-deletes; transactions stay in history (shown as "Removed wallet"). Its share of the current budget is redistributed to the other in-budget wallets in proportion; the last takes the rounding leftover so the split stays exactly 100. |
| `renameWallet(id, name)` | Writes only if the name changed. |
| `setWalletTheme(id, theme)` | Drops empty colours; an empty theme clears it. |
| `setWalletBalance(walletId, target)` | Records `target − balance` as an `adjustment`, note "Balance correction". Nothing if 0. |
| `logExpense({walletId, amount, note, date?, countsTowardBudget?})` | Stores `−|amount|`, kind `expense`; date defaults to today, counting to `true`. |
| `logIncome({walletId, amount, note?, date?})` | Stores `+|amount|`, kind `income`, never counted. |
| `deleteTransaction(id)` | Soft delete. |
| `saveBudget(budget)` | Insert or replace a period; a new one gets a fresh id. |
| `listTransactions(limit = 300)` | Newest day first; within a day, most recently changed first. |

### Exempt toggle (plan editor)

Toggling a wallet out sets its share to 0. Toggling it back in gives it `max(1, 100 − sum of others)`.

---

## 4. Screens and UX flows

### First run (Setup)

1. **Your wallets.** Currency picker (pre-filled from region). Rows of name + current amount, starting with "Cash" and "Bank". "+ Wallet", remove (✕, disabled for the last row). Needs at least one named wallet. Each becomes a wallet with a starting-balance adjustment. Skipped if wallets exist.
2. **Your budget.** Total, days (default 14), and a percent split per wallet (pre-split evenly, last takes the leftover). Shows "Your budget wallets hold ₱X" before you type. Saves with `startDate = today`.

Setup shows whenever there are no wallets or no budget.

### Main screen (`/wallet`)

- **Mode switch:** ALLOWANCE (left to spend today) / TOTAL BALANCE (what the wallet holds now).
- **Wallet dial:** an endless horizontal loop of "All" + every wallet. Drag, fling, scroll, tap or arrow keys; ticks once per label crossing the centre, with momentum and detents. The selected wallet is remembered when visiting Calendar/Manage.
- **Big number:** rolls between values (`RollingNumber`). Tap it to log.
- **Day boxes:** one box per budget day; filled = started, today marked. Label: "Day N of M", "Starts YYYY-MM-DD" or "Budget ended". Tap → calendar.
- **Manage →** link at the bottom.
- **Colours:** the whole Wallet section takes the selected wallet's theme, fading over 350 ms.
- **Desktop:** typing a digit, `.`, `+` or `-` anywhere opens entry pre-filled.

### Amount entry (`Entry`)

- Full screen; one hidden input keeps focus so phone keyboards and PC keyboards both work.
- Sign defaults to expense (−). Typing `+` or `-` anywhere flips the sign; a button flips it too.
- Max 9 digits, max 2 decimals. The whole part rolls; decimals sit after it as plain text.
- **Drag up/down or scroll to adjust:** 18 px per notch (wheel: 40). Speed picks the step: < 0.5 px/ms → 1, < 1.4 → 10, else 100. Bigger steps snap to round numbers (23 → 30 / 100). Numbers ending in 0 or 5 are "sticky" detents (2.5× the drag to leave, slow drags only) with a heavier click. A badge shows "+10" etc. for 700 ms.
- The wallet dial is present here too; you can switch wallet while entering.
- Errors: "Swipe to a wallet first" (when on All), "Type an amount".
- Enter submits, Escape cancels. Shows the wallet's balance greyed.
- Logging from the main screen sets `note = ''`.

### Calendar (`/wallet/calendar`)

1. **Month grid** (Monday first), flip with arrows or a sideways swipe. Range: from the earliest transaction or budget month to the latest budget end. Each day shows compact spending. Legend: budget day, passed, over, today. A day is "over" when its budget spending exceeds its allowance.
   - Per-day allowance comes from the latest-starting budget that covers that day: `sum over in-budget wallets of total × share / 100 / days`. (Note: this differs slightly from the main screen, which always uses the current budget.)
2. **Picked day:** per-wallet "left of ₱X", or "Not part of a budget. Spent ₱X · in ₱Y". Lists that day's transactions (adjustments hidden), note or "Spent"/"Money in" + wallet name.
3. **Month in numbers:** Spent, In, Net; spending-per-day bar chart; Avg/day (over elapsed days); Biggest day; "On budget N of M days"; spending by wallet (sorted, bars).
4. **Plan:** edit total, days, start date and each wallet's share or exempt toggle, with ₱/day and total per wallet. The calendar previews the draft live; nothing saves until Save (with "saved" flash for 1.5 s).

### Manage (`/wallet/manage`)

- **Budget tab:** the plan editor.
- **Wallets tab:** rename, set balance (as a correction), colours toggle (bg/ink/accent), remove, add a wallet.
- **History tab:** everything logged, grouped by day ("Today", "Yesterday", "Mon, Oct 27"). Filters: wallet, type (All / Spent / Money in / Balance changes), period (this budget / all time). Shows totals Spent and Money in for the filter. Delete is two-tap (arm, then confirm).

### Feedback

A 12 ms noise "click" plus a haptic tap on every meaningful change (`tick()`, heavier on detents and saves). Both have on/off switches in Settings. Respect reduced-motion.

---

## 5. Theming

- App palette: three chosen colours `bg`, `ink` (text), `accent`; everything else is mixed from them (`dim` = 42% ink, `faint` = 14% ink).
- Three layers, most specific wins: global → tool look (`theme:finance`) → the selected wallet's `theme`.
- Wallet palette rule: `bg` falls back to the app; if the wallet sets `bg` but not `ink`, ink is auto (black or white for contrast); `accent` falls back to the app.
- Default look: black background, white ink, white accent, Barlow Condensed 900 italic display type, uppercase, line-height 0.95. Other display fonts: Big Shoulders, Archivo Black, Anton (each with a width factor that scales sizes). Body/reading font: Space Grotesk. Mono: JetBrains Mono.
- Colour swatches: accent `#ffffff #ff1a1a #1e6bff #ffd60a #2bd96b #ff4fa3 #ff7a00 #8b5cf6`; bg `#000000 #0b1020 #1a0505 #0d1a12 #efede8 #ffffff #ffd60a #1e6bff`; ink `#ffffff #000000 #efede8 #c8ff00 #ff1a1a`.
- Danger colour `#ff3b30`.

---

## 6. Sync and backup formats (for the importer)

### Snapshot (the JSON inside both sync and backups)

```json
{
  "format": 1,
  "deviceId": "…",
  "at": 1730000000000,
  "tools": {
    "finance": {
      "wallets": [ …Wallet records, tombstones included… ],
      "transactions": [ … ],
      "budgets": [ … ]
    },
    "focus": { … }, "fidget": { … }
  },
  "settings": {
    "currency": { "value": "PHP", "at": 1730000000000 },
    "looks": { … per-tool looks, e.g. theme:finance … }
  }
}
```

Merge rule: per record id, the newer `updatedAt` wins. Settings use the same rule via their `at`. Ignore records missing `id` (string), `updatedAt` (number) or `deleted` (boolean). The importer only needs `tools.finance` and `settings.currency`.

### `.escb` backup file

```
bytes 0-3    "ESCB"           signature
byte  4      version = 1
byte  5      key method = 1   (PBKDF2-SHA256)
bytes 6-9    rounds           big-endian u32 (currently 600,000)
bytes 10-25  salt             16 bytes
bytes 26-37  IV               12 bytes
bytes 38-…   AES-256-GCM( gzip( JSON snapshot ) )
```

- Key = PBKDF2-SHA256(password, salt, rounds) → 256-bit AES-GCM key.
- The 38-byte header is passed as AES-GCM additional data (tamper-sealed).
- Decrypt failure = wrong password or damaged file.
- File name: `escillex-backup-YYYY-MM-DD.escb`.

Rust crates for the importer: `pbkdf2` + `sha2`, `aes-gcm`, `flate2` (gzip), `serde_json`.

### Live sync (being replaced)

Pairing code (15 random bytes, Crockford base32, 24 chars like `K7Q2-9XHM-4P8D-TW3R-ZC6N-HJ5V`) → HKDF → a mailbox id (sent to the server) and an AES-GCM key (never leaves the device). Snapshots are JSON → gzip → AES-GCM, swapped through a Cloudflare Durable Object that stores nothing. The native app replaces this with self-hosted sync; you only need the importer.

---

## 7. Known gaps and new features

**The gap:** non-daily spending can only be excluded (`countsTowardBudget = false`), not planned or tracked.

**Pots** — a second axis next to wallets. A wallet says where the money is; a pot says what it's for.

- **Bill:** fixed amount on a schedule (rent, subscriptions).
- **Goal / sinking fund:** a target by a date (₱6,000 phone by March).
- **One-off:** set aside once, spent once (₱1,500 gift this month).

Pots come off the top of the budget:

```
daily allowance = (budget total − reserved for pots this period) / days
```

Each transaction goes to daily (`potId = null`) or to a pot, replacing `countsTowardBudget`.

```
Pot         { name, kind: bill | goal | oneoff, target, cadence?, dueDate?, rollover }
Transaction { …, potId: string | null }
Allocation  { potId, budgetId, amount }   // what this period reserved for the pot
```

Pot balance = allocations − spending, always computed. `rollover` decides whether leftovers stay in the pot or return to daily.

**Open questions**

- [ ] What are the non-daily budgets mostly: bills, goals, or one-offs?
- [ ] Does pot money come from the same budget total, or separate income?
- [ ] Leftovers: carry forward or return to daily?
- [ ] Pots tied to a wallet, or free-floating?
- [ ] Keep the fixed daily allowance, or add rollover?
- [ ] Unify the calendar and main screen allowance rules (latest covering plan vs current plan)?
- [ ] Should notes be enterable from the entry screen? (Currently always empty from there.)

---

## 8. Proposed architecture

```
wallet/
  crates/
    wallet-core/      model, budget maths, pots, validation (pure Rust, no I/O)
    wallet-store/     SQLite via rusqlite, migrations, outbox
    wallet-sync/      HTTP client for the self-hosted server, LWW merge
    wallet-import/    .escb + snapshot JSON importer
  app/                Makepad 2.0 app (Splash UI), Android + desktop
  server/             small sync server (axum + SQLite or Postgres) for VPS/home
```

### SQLite schema (starting point)

```sql
CREATE TABLE wallets (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, color TEXT NOT NULL,
  ord INTEGER NOT NULL, theme TEXT,             -- JSON {bg,ink,accent} or NULL
  updated_at INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE transactions (
  id TEXT PRIMARY KEY, wallet_id TEXT NOT NULL,
  amount INTEGER NOT NULL,                      -- hundredths, signed
  kind TEXT NOT NULL CHECK (kind IN ('expense','income','adjustment')),
  date TEXT NOT NULL,                           -- YYYY-MM-DD local
  note TEXT NOT NULL DEFAULT '',
  pot_id TEXT,                                  -- NULL = daily
  counts_toward_budget INTEGER NOT NULL DEFAULT 0, -- kept for imported data
  updated_at INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX tx_wallet ON transactions(wallet_id);
CREATE INDEX tx_date ON transactions(date);
CREATE TABLE budgets (
  id TEXT PRIMARY KEY, total INTEGER NOT NULL, start_date TEXT NOT NULL,
  days INTEGER NOT NULL, split TEXT NOT NULL,   -- JSON {walletId: percent}
  updated_at INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE pots (
  id TEXT PRIMARY KEY, name TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('bill','goal','oneoff')),
  target INTEGER NOT NULL, cadence TEXT, due_date TEXT, rollover INTEGER NOT NULL,
  updated_at INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE allocations (
  id TEXT PRIMARY KEY, pot_id TEXT NOT NULL, budget_id TEXT NOT NULL, amount INTEGER NOT NULL,
  updated_at INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE outbox (seq INTEGER PRIMARY KEY AUTOINCREMENT, tbl TEXT NOT NULL, id TEXT NOT NULL);
```

### Sync (self-hosted, single user)

1. Every local write updates the row and appends `(table, id)` to `outbox`.
2. Push: send changed rows to `POST /sync/push`; the server keeps the newer `updated_at` per id.
3. Pull: `GET /sync/pull?since=<server cursor>` returns rows changed since; apply with the same LWW rule; save the cursor.
4. Clear outbox entries once acknowledged.
5. Auth: a static bearer token; reach the server over Tailscale (`tailscale serve` for HTTPS).
6. Server backups: scheduled dump of its database.

Never sync computed values (balances, allowances, pot balances).

### Makepad notes

Full reference: [makepad-docs.md](makepad-docs.md) (setup, app skeleton, Splash rules, widgets, events, Wallet mapping).

- Makepad 2.0 (Feb 2026): UI in Splash via `script_mod!`, live-editable without recompiling; Rust underneath.
- Android build (from the makepad checkout): `cargo run -p cargo-makepad --release -- android toolchain-install`, then `cargo run -p cargo-makepad --release -- android --package-name=com.escillex.wallet --app-label="Wallet" run -p <app>`.
- Look at the official Finance demo app on makepad.nl first.
- Prototype the risky screens first: amount entry (text input, drag-to-adjust), the endless wallet dial, the calendar grid.
- Haptics, file picker for `.escb`, and notifications may need Android glue.

---

## 9. Tests to port and first steps

### Behaviour tests worth writing in `wallet-core` first

- [ ] Balance = sum of live transactions; tombstones ignored.
- [ ] `budgetSummary`: allowance per wallet, "All" = sum, 0 outside period, rounding only at the end.
- [ ] Day number < 1 and > days.
- [ ] Exempt wallets excluded from allowance and their expenses not counted.
- [ ] `deleteWallet` redistributes the share proportionally and the split stays exactly 100.
- [ ] `setWalletBalance` writes the difference, nothing when equal.
- [ ] Budget cap: `maxBudget = held + spentSoFar`.
- [ ] Split validation = exactly 100; total > 0; days ≥ 1.
- [ ] `addDays` across month and year ends, local dates.
- [ ] `toCentavos` rounding (e.g. 0.1 + 0.2 cases).
- [ ] LWW merge: newer `updatedAt` wins; tombstones propagate; merge order doesn't matter.
- [ ] `.escb` round-trip: encode in TS, decode in Rust, using a fixture file.
- [ ] Calendar stats: on-budget days, average over elapsed days, biggest day.

### First steps

1. [ ] Export a `.escb` backup from the playground Wallet as a fixture (and a test one with fake data).
2. [ ] Create the cargo workspace with `wallet-core` and port `data.ts` logic + the tests above.
3. [ ] Add `wallet-store` (rusqlite + migrations) with the schema above.
4. [ ] Add `wallet-import` and confirm it reads your real backup.
5. [ ] Install Makepad 2.0 and its Android toolchain; run the Finance demo on your phone.
6. [ ] Prototype the amount entry and wallet dial in Splash.
7. [ ] Build the main screen, then Calendar, then Manage.
8. [ ] Decide the pots design (open questions above), then add it to core and UI.
9. [ ] Stand up the sync server on a VPS or at home behind Tailscale.
10. [ ] Retire the playground Wallet once the native one has your data.
