//! Two friends on the same computer find each other through the LocalSend
//! announcements (the local network) and connect, even where iroh's relays
//! cannot be reached; then a file goes from one to the other.
//!
//!   cargo run --release --example nearby_test -- <workdir>
//!   cargo run --release --example nearby_test -- <workdir> sans-local
//!
//! `sans-local` starts no LocalSend: on a network that blocks the relays,
//! the two then stay apart (for comparison). Set P2PSHARE_NO_INTERNET to
//! act as such a network: no relay, no directory.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::SecretKey;
use p2pshare_lib::localsend::{Hooks, Identity, LocalSend};
use p2pshare_lib::p2p::{Dir, Node, Options, Status};
use p2pshare_lib::store::JsonStore;

const TIMEOUT: Duration = Duration::from_secs(60);
const FILE_SIZE: usize = 20 << 20;

async fn start(dir: &Path, name: &str, secret: SecretKey) -> (Node, Identity) {
    std::fs::create_dir_all(dir).unwrap();
    let identity = Identity::load_or_create(&dir.join("localsend.json")).expect("identity");
    let downloads = dir.join("downloads");
    let node = Node::start(Options {
        secret,
        friends: JsonStore::load(dir.join("friends.json"), Vec::new()),
        invites: JsonStore::load(dir.join("invites.json"), Vec::new()),
        messages_dir: dir.join("messages"),
        download_dir: Arc::new(move || downloads.clone()),
        on_event: Arc::new(|_| {}),
        groups_dir: dir.join("groups"),
        nickname: name.to_string(),
        relay: None,
        lan: Some(identity.clone()),
    })
    .await
    .expect("node");
    (node, identity)
}

fn hooks() -> Hooks {
    Hooks {
        on_text: Arc::new(|_, _| {}),
        on_devices: Arc::new(|| {}),
    }
}

fn route(n: &Node, id: &str) -> String {
    n.friends()
        .into_iter()
        .find(|f| f.id == id)
        .map(|f| match (f.online, f.route) {
            (false, _) => "hors ligne".to_string(),
            (true, r) => r.unwrap_or("?").to_string(),
        })
        .unwrap_or_default()
}

async fn wait(what: &str, mut ok: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while !ok() {
        if start.elapsed() > TIMEOUT {
            println!("  ÉCHEC  {what} (après {}s)", TIMEOUT.as_secs());
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    println!("  ok  {what} ({:.1}s)", start.elapsed().as_secs_f64());
    true
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let work = PathBuf::from(args.next().expect("usage: nearby_test <workdir> [sans-local]"));
    let local = args.next().as_deref() != Some("sans-local");
    let _ = std::fs::remove_dir_all(&work);
    let (sa, sb) = (SecretKey::generate(), SecretKey::generate());
    let (ida, idb) = (sa.public().to_string(), sb.public().to_string());
    let (a, ia) = start(&work.join("alice"), "alice", sa).await;
    let (b, ib) = start(&work.join("bob"), "bob", sb).await;
    a.add_friend(&idb, "Bob").unwrap();
    b.add_friend(&ida, "Alice").unwrap();
    let started = Instant::now();

    // Kept alive for the test.
    let _services = if local {
        let la = LocalSend::start(a.clone(), &ia, "nearby-test alice", hooks()).await.expect("localsend");
        let lb = LocalSend::start(b.clone(), &ib, "nearby-test bob", hooks()).await.expect("localsend");
        println!("LocalSend démarré (ports {} et {})", la.status().port, lb.status().port);
        Some((la, lb))
    } else {
        println!("Sans LocalSend");
        None
    };

    let connected = wait("Alice et Bob connectés", || {
        a.friends().iter().any(|f| f.id == idb && f.online)
            && b.friends().iter().any(|f| f.id == ida && f.online)
    })
    .await;
    let relay = |n: &Node| match n.relay_status() {
        Some(r) if r.connected => format!("connecté à {}", r.url),
        Some(r) => format!("injoignable ({})", r.error.unwrap_or_default()),
        None => "aucun".into(),
    };
    println!("Relais d'Alice : {}", relay(&a));
    println!("Relais de Bob : {}", relay(&b));
    if !connected {
        std::process::exit(1);
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    println!(
        "Chemin vu par Alice : {} · par Bob : {} ({:.1}s après le départ)",
        route(&a, &idb),
        route(&b, &ida),
        started.elapsed().as_secs_f64()
    );

    let data: Vec<u8> = (0..FILE_SIZE as u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    let file = work.join("envoi.bin");
    std::fs::write(&file, &data).unwrap();
    a.send_files(&idb, &[file]);
    let mut routes = Vec::new();
    let ok = wait("fichier de 20 Mo reçu par Bob", || {
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
    if ok {
        let got = b.transfers().into_iter().find(|t| t.status == Status::Completed).unwrap();
        let same = std::fs::read(got.saved_path.unwrap()).unwrap() == data;
        println!("Chemin du fichier : {routes:?} · contenu identique : {same}");
    }
    for n in [&a, &b] {
        n.shutdown().await;
    }
    std::process::exit(if ok { 0 } else { 1 });
}
