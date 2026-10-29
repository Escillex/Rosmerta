//! Peer-to-peer sync over iroh. Every device is an equal peer: paired
//! devices swap the history entries the other is missing, and each side
//! merges them newest-wins (see `wallet_store::Store::apply_remote`).
//!
//! One sync is one bi-directional stream, dialer `D`, acceptor `A`:
//!
//! ```text
//! D → A  Hello, Entries      what A hasn't confirmed from D
//! A → D  Hello, Entries      A applied D's; now A's for D
//! D → A  Got                 D applied A's
//! A closes; both mark what the other confirmed.
//! ```

use anyhow::{Context, Result, bail, ensure};
use iroh::endpoint::{Connection, RecvStream, SendStream, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use serde::{Deserialize, Serialize};
use wallet_core::Book;
use wallet_store::{Applied, HistoryEntry, Peer, Store};

pub const ALPN: &[u8] = b"rosmerta/sync/1";
/// One message can't be bigger than this (a few years of history is far smaller).
const MAX_MSG: u32 = 64 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
enum Msg {
    Hello { device: String },
    Entries(Vec<HistoryEntry>),
    Got,
}

pub struct Node {
    pub endpoint: Endpoint,
}

/// How the endpoint reaches peers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// LAN, hole punching and n0's relays: works anywhere.
    Internet,
    /// Direct addresses only, no relays or lookups (tests, offline LAN).
    DirectOnly,
}

impl Node {
    /// Binds this device's endpoint. Its key is made once and kept in the store.
    pub async fn start(store: &Store, reach: Reach) -> Result<Node> {
        let key = match store.meta("iroh_secret")? {
            Some(hex) => SecretKey::from_bytes(&from_hex(&hex).context("stored iroh key is damaged")?),
            None => {
                let k = SecretKey::generate();
                store.set_meta("iroh_secret", &to_hex(&k.to_bytes()))?;
                k
            }
        };
        let builder = match reach {
            Reach::Internet => Endpoint::builder(presets::N0),
            Reach::DirectOnly => Endpoint::builder(presets::Minimal),
        };
        let endpoint = builder.secret_key(key).alpns(vec![ALPN.to_vec()]).bind().await?;
        Ok(Node { endpoint })
    }

    /// What a peer pairs with (the QR code later).
    pub fn id(&self) -> EndpointId {
        self.endpoint.id()
    }

    pub fn addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    /// Dials a paired peer and syncs. `addr` lets callers pass direct
    /// addresses; otherwise the peer's node id is looked up.
    pub async fn sync_with(&self, store: &mut Store, book: &mut Book, peer: &Peer, addr: Option<EndpointAddr>, now: i64) -> Result<Applied> {
        let id: EndpointId = peer.node.parse().context("bad peer node id")?;
        let conn = match addr {
            Some(a) => self.endpoint.connect(a, ALPN).await?,
            None => self.endpoint.connect(id, ALPN).await?,
        };
        let (mut send, mut recv) = conn.open_bi().await?;

        write(&mut send, &Msg::Hello { device: store.device().to_string() }).await?;
        let (mine, top) = store.outgoing(&peer.device)?;
        write(&mut send, &Msg::Entries(mine)).await?;

        expect_hello(&mut recv, &peer.device).await?;
        let Msg::Entries(theirs) = read(&mut recv).await? else { bail!("expected entries") };
        store.mark_sent(&peer.device, top)?; // their reply means ours landed
        let applied = store.apply_remote(book, &peer.device, &theirs)?;
        write(&mut send, &Msg::Got).await?;
        send.finish()?;
        conn.closed().await; // the acceptor closes once it has our Got
        store.finish_sync(&peer.device, now)?;
        Ok(applied)
    }

    /// Waits for one incoming sync from a paired peer and answers it.
    /// Unpaired devices are refused.
    pub async fn accept_one(&self, store: &mut Store, book: &mut Book, now: i64) -> Result<Applied> {
        let incoming = self.endpoint.accept().await.context("endpoint closed")?;
        let conn = incoming.await?;
        let result = answer(&conn, store, book, now).await;
        conn.close(0u32.into(), if result.is_ok() { b"done" } else { b"refused" });
        result
    }

    pub async fn close(self) {
        self.endpoint.close().await;
    }
}

async fn answer(conn: &Connection, store: &mut Store, book: &mut Book, now: i64) -> Result<Applied> {
    let node = conn.remote_id().to_string();
    let peer = store.peer_by_node(&node)?.with_context(|| format!("{node} isn't paired"))?;
    let (mut send, mut recv) = conn.accept_bi().await?;

    expect_hello(&mut recv, &peer.device).await?;
    let Msg::Entries(theirs) = read(&mut recv).await? else { bail!("expected entries") };
    let applied = store.apply_remote(book, &peer.device, &theirs)?;

    write(&mut send, &Msg::Hello { device: store.device().to_string() }).await?;
    let (mine, top) = store.outgoing(&peer.device)?;
    write(&mut send, &Msg::Entries(mine)).await?;
    let Msg::Got = read(&mut recv).await? else { bail!("expected got") };
    store.mark_sent(&peer.device, top)?;
    store.finish_sync(&peer.device, now)?;
    Ok(applied)
}

async fn expect_hello(recv: &mut RecvStream, device: &str) -> Result<()> {
    let Msg::Hello { device: got } = read(recv).await? else { bail!("expected hello") };
    ensure!(got == device, "peer says it's {got}, paired as {device}");
    Ok(())
}

async fn write(send: &mut SendStream, msg: &Msg) -> Result<()> {
    let bytes = serde_json::to_vec(msg)?;
    send.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    send.write_all(&bytes).await?;
    Ok(())
}

async fn read(recv: &mut RecvStream) -> Result<Msg> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len);
    ensure!(len <= MAX_MSG, "message too big ({len} bytes)");
    let mut buf = vec![0u8; len as usize];
    recv.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn from_hex(s: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    if s.len() != 64 {
        return None;
    }
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wallet_core::{Date, Env};

    struct E {
        now: i64,
        next: u32,
        prefix: &'static str,
    }

    impl Env for E {
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

    /// Two real iroh endpoints on this machine, direct addresses only.
    #[tokio::test(flavor = "multi_thread")]
    async fn two_devices_sync_over_iroh() -> Result<()> {
        let (mut laptop, mut phone) = (Store::open_in_memory()?, Store::open_in_memory()?);
        let (mut lbook, mut pbook) = (Book::default(), Book::default());
        let ln = Node::start(&laptop, Reach::DirectOnly).await?;
        let pn = Node::start(&phone, Reach::DirectOnly).await?;
        laptop.pair(phone.device(), &pn.id().to_string(), "phone")?;
        phone.pair(laptop.device(), &ln.id().to_string(), "laptop")?;

        let mut le = E { now: 1_000, next: 0, prefix: "lap" };
        let cash = lbook.create_wallet(&mut le, "Cash", "#fff", 10_000);
        lbook.log_expense(&mut le, &cash, 2_000, "", None, None);
        laptop.commit(&mut lbook)?;
        let mut pe = E { now: 1_100, next: 0, prefix: "pho" };
        pbook.create_wallet(&mut pe, "Bank", "#fff", 30_000);
        phone.commit(&mut pbook)?;

        let peer = laptop.peers()?.remove(0);
        let addr = pn.addr();
        let accept = async { pn.accept_one(&mut phone, &mut pbook, 2_000).await };
        let dial = async { ln.sync_with(&mut laptop, &mut lbook, &peer, Some(addr), 2_000).await };
        let (got_phone, got_laptop) = tokio::join!(accept, dial);
        let (got_phone, got_laptop) = (got_phone?, got_laptop?);

        assert_eq!(got_phone.new_entries, 3); // wallet + starting balance + expense
        assert_eq!(got_laptop.new_entries, 2); // wallet + starting balance
        assert_eq!(lbook.total_balance(), 38_000);
        assert_eq!(pbook.total_balance(), 38_000);
        assert_eq!(phone.load()?.total_balance(), 38_000);
        assert_eq!(laptop.peers()?[0].last_sync, 2_000);
        // Both sides confirmed: nothing left to send.
        assert!(laptop.outgoing(phone.device())?.0.is_empty());
        assert!(phone.outgoing(laptop.device())?.0.is_empty());

        ln.close().await;
        pn.close().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unpaired_devices_are_refused() -> Result<()> {
        let (mut a, mut b) = (Store::open_in_memory()?, Store::open_in_memory()?);
        let (mut abook, mut bbook) = (Book::default(), Book::default());
        let an = Node::start(&a, Reach::DirectOnly).await?;
        let bn = Node::start(&b, Reach::DirectOnly).await?;
        // a thinks it's paired with b; b never paired a.
        a.pair(b.device(), &bn.id().to_string(), "b")?;
        let peer = a.peers()?.remove(0);
        let addr = bn.addr();
        let (accepted, dialed) = tokio::join!(
            bn.accept_one(&mut b, &mut bbook, 1),
            an.sync_with(&mut a, &mut abook, &peer, Some(addr), 1)
        );
        assert!(accepted.unwrap_err().to_string().contains("isn't paired"));
        assert!(dialed.is_err());
        an.close().await;
        bn.close().await;
        Ok(())
    }

    #[test]
    fn hex_round_trip() {
        let b = [7u8; 32];
        assert_eq!(from_hex(&to_hex(&b)), Some(b));
        assert_eq!(from_hex("zz"), None);
    }
}
