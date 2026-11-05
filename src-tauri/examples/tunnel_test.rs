//! Alice is on a network that blocks iroh's relays and directory but not
//! her own relay (as eduroam); Bob, elsewhere, uses iroh's public relays
//! only. Alice reaches Bob's public relay through hers (the tunnel), finds
//! Bob through her relay's directory (which asks iroh's), and they send a
//! file.
//!
//!   cargo run --example tunnel_test -- <workdir> <p2p-relay binary>
//!
//! The binary is P2P-Sender-relay's `p2p-relay`: the test starts it on
//! 127.0.0.1:3340 with a token. Needs the internet. Neither has direct
//! paths (P2PSHARE_RELAY_ONLY), so the file goes through the relays.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::SecretKey;
use p2pshare_lib::p2p::{CustomRelay, Dir, Node, Options, Status};
use p2pshare_lib::store::JsonStore;

const RELAY: &str = "http://127.0.0.1:3340";
const TOKEN: &str = "jeton-du-test";
const FILE_SIZE: usize = 20 << 20;

async fn start(dir: &Path, name: &str, secret: SecretKey, relay: Option<CustomRelay>) -> Node {
    std::fs::create_dir_all(dir).unwrap();
    let downloads = dir.join("downloads");
    Node::start(Options {
        kept: JsonStore::read_only(Default::default(), Vec::new()),
        secret,
        friends: JsonStore::load(dir.join("friends.json"), Vec::new()),
        invites: JsonStore::load(dir.join("invites.json"), Vec::new()),
        messages_dir: dir.join("messages"),
        download_dir: Arc::new(move || downloads.clone()),
        on_event: Arc::new(|_| {}),
        groups_dir: dir.join("groups"),
        nickname: name.to_string(),
        relay,
        lan: None,
    })
    .await
    .expect("node")
}

fn home(n: &Node) -> String {
    match n.relay_status() {
        Some(r) if r.connected => if r.custom { "son relais".into() } else { format!("relais public {}", r.url) },
        Some(r) => format!("pas connecté ({})", r.error.unwrap_or_default()),
        None => "aucun".into(),
    }
}

async fn wait(what: &str, limit: Duration, mut ok: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while !ok() {
        if start.elapsed() > limit {
            println!("  ÉCHEC  {what} (après {}s)", limit.as_secs());
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("  ok  {what} ({:.1}s)", start.elapsed().as_secs_f64());
    true
}

struct Relay(Child);

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let work = PathBuf::from(args.next().expect("usage: tunnel_test <workdir> <p2p-relay>"));
    let relay_bin = PathBuf::from(args.next().expect("usage: tunnel_test <workdir> <p2p-relay>"));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    // Its output in a file: nothing holds this program's output open.
    let log = std::fs::File::create(work.join("relay.log")).unwrap();
    // Both without direct paths: everything goes through relays.
    std::env::set_var("P2PSHARE_RELAY_ONLY", "1");
    // Stopped before each exit: `process::exit` runs no destructor.
    let relay = Relay(
        Command::new(&relay_bin)
            .env("LISTEN_ADDR", "127.0.0.1:3340")
            .env("RELAY_TOKEN", TOKEN)
            .env("RUST_LOG", "warn")
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("p2p-relay"),
    );
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (sa, sb) = (SecretKey::generate(), SecretKey::generate());
    let (ida, idb) = (sa.public().to_string(), sb.public().to_string());

    // Alice: iroh's relays and directory "blocked", her relay reachable.
    std::env::set_var("P2PSHARE_BLOCK_PUBLIC", "1");
    let a = start(&work.join("alice"), "alice", sa, Some(CustomRelay::parse(RELAY, TOKEN).unwrap())).await;
    std::env::remove_var("P2PSHARE_BLOCK_PUBLIC");
    // Bob: public relays only, no relay of his own.
    let b = start(&work.join("bob"), "bob", sb, None).await;

    let homes = wait("Alice sur son relais, Bob sur un relais public", Duration::from_secs(60), || {
        a.relay_status().is_some_and(|r| r.connected && r.custom)
            && b.relay_status().is_some_and(|r| r.connected)
    })
    .await;
    println!("     Alice : {} · Bob : {}", home(&a), home(&b));

    a.add_friend(&idb, "Bob").unwrap();
    b.add_friend(&ida, "Alice").unwrap();
    let connected = wait("Alice et Bob connectés", Duration::from_secs(120), || {
        a.friends().iter().any(|f| f.id == idb && f.online)
            && b.friends().iter().any(|f| f.id == ida && f.online)
    })
    .await;
    if !connected {
        for n in [&a, &b] {
            n.shutdown().await;
        }
        drop(relay);
        std::process::exit(1);
    }

    let data: Vec<u8> = (0..FILE_SIZE as u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    let file = work.join("envoi.bin");
    std::fs::write(&file, &data).unwrap();
    let sent = Instant::now();
    a.send_files(&idb, &[file]);
    let mut routes = Vec::new();
    let received = wait("fichier de 20 Mo reçu par Bob", Duration::from_secs(300), || {
        for t in b.transfers() {
            if t.dir == Dir::In && t.status == Status::Pending {
                b.respond_to_offer(&t.id, true);
            }
            if let Some(r) = t.route {
                if !routes.contains(&r) {
                    routes.push(r);
                }
            }
        }
        b.transfers().iter().any(|t| t.dir == Dir::In && t.status == Status::Completed)
    })
    .await;
    if received {
        let secs = sent.elapsed().as_secs_f64();
        let got = b.transfers().into_iter().find(|t| t.status == Status::Completed).unwrap();
        let same = std::fs::read(got.saved_path.unwrap()).unwrap() == data;
        println!(
            "     chemin du fichier : {routes:?} · {:.1} Mo/s · contenu identique : {same}",
            FILE_SIZE as f64 / secs / 1e6
        );
    }
    for n in [&a, &b] {
        n.shutdown().await;
    }
    drop(relay);
    std::process::exit(if homes && received { 0 } else { 1 });
}
