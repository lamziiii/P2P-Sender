//! Our own relay comes first: while it does not answer, the public relays
//! stand in; once it answers, it is the home relay again, and two friends
//! who use it exchange through it.
//!
//!   cargo run --example relay_test -- <workdir> <p2p-relay binary>
//!
//! The binary is P2P-Sender-relay's `p2p-relay`: the test starts it on
//! 127.0.0.1:3340 halfway. Needs the internet (public relays). With
//! P2PSHARE_RELAY_ONLY set, nodes have no direct path: the file then goes
//! through the relay.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::SecretKey;
use p2pshare_lib::p2p::{CustomRelay, Dir, Node, Options, Status};
use p2pshare_lib::store::JsonStore;

const RELAY: &str = "http://127.0.0.1:3340";
const FILE_SIZE: usize = 20 << 20;

async fn start(dir: &Path, name: &str, secret: SecretKey) -> Node {
    std::fs::create_dir_all(dir).unwrap();
    let downloads = dir.join("downloads");
    Node::start(Options {
        secret,
        friends: JsonStore::load(dir.join("friends.json"), Vec::new()),
        invites: JsonStore::load(dir.join("invites.json"), Vec::new()),
        messages_dir: dir.join("messages"),
        download_dir: Arc::new(move || downloads.clone()),
        on_event: Arc::new(|_| {}),
        groups_dir: dir.join("groups"),
        nickname: name.to_string(),
        relay: Some(CustomRelay::parse(RELAY, "").unwrap()),
        lan: None,
    })
    .await
    .expect("node")
}

/// "votre relais" or "relais public <url>", once connected.
fn home(n: &Node) -> Option<String> {
    let r = n.relay_status()?;
    r.connected.then(|| if r.custom { "votre relais".into() } else { format!("relais public {}", r.url) })
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
    let work = PathBuf::from(args.next().expect("usage: relay_test <workdir> <p2p-relay>"));
    let relay_bin = PathBuf::from(args.next().expect("usage: relay_test <workdir> <p2p-relay>"));
    let _ = std::fs::remove_dir_all(&work);
    let (sa, sb) = (SecretKey::generate(), SecretKey::generate());
    let (ida, idb) = (sa.public().to_string(), sb.public().to_string());

    println!("1. Votre relais éteint");
    let a = start(&work.join("alice"), "alice", sa).await;
    let fallback = wait("Alice passe par un relais public en attendant", Duration::from_secs(90), || {
        home(&a).is_some_and(|h| h != "votre relais")
    })
    .await;
    println!("     Alice : {}", home(&a).unwrap_or_default());

    println!("2. Votre relais allumé");
    // Stopped before each exit: `process::exit` runs no destructor.
    let relay = Relay(
        Command::new(&relay_bin)
            .env("LISTEN_ADDR", "127.0.0.1:3340")
            .env_remove("RELAY_TOKEN")
            .env("RUST_LOG", "warn")
            .spawn()
            .expect("p2p-relay"),
    );
    let back = wait("Alice revient sur votre relais", Duration::from_secs(150), || {
        home(&a).as_deref() == Some("votre relais")
    })
    .await;

    println!("3. Bob, ami d'Alice, avec le même relais");
    let b = start(&work.join("bob"), "bob", sb).await;
    let bob_home = wait("Bob est sur votre relais", Duration::from_secs(60), || {
        home(&b).as_deref() == Some("votre relais")
    })
    .await;
    a.add_friend(&idb, "Bob").unwrap();
    b.add_friend(&ida, "Alice").unwrap();
    let connected = wait("Alice et Bob connectés", Duration::from_secs(60), || {
        a.friends().iter().any(|f| f.id == idb && f.online)
            && b.friends().iter().any(|f| f.id == ida && f.online)
    })
    .await;
    if !connected {
        drop(relay);
        std::process::exit(1);
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let route = a.friends().into_iter().find(|f| f.id == idb).and_then(|f| f.route);
    println!("     chemin vu par Alice : {}", route.unwrap_or("?"));

    let data: Vec<u8> = (0..FILE_SIZE as u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    let file = work.join("envoi.bin");
    std::fs::write(&file, &data).unwrap();
    let sent = Instant::now();
    a.send_files(&idb, &[file]);
    let mut routes = Vec::new();
    let received = wait("fichier de 20 Mo reçu par Bob", Duration::from_secs(180), || {
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
    std::process::exit(if fallback && back && bob_home && received { 0 } else { 1 });
}
