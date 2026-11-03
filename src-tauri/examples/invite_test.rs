//! Invitation links between real nodes on this computer, without internet
//! (each one is told where the other is, as the local network would):
//!
//!   1. Alice makes a link, Bob accepts it: both are friends, connected.
//!   2. Carol accepts the same link: it is used up, Alice turns her away
//!      and Carol's app shows that the link no longer works.
//!
//!   cargo run --example invite_test -- <workdir>

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::SecretKey;
use p2pshare_lib::invite;
use p2pshare_lib::p2p::{Node, Options};
use p2pshare_lib::store::JsonStore;

const TIMEOUT: Duration = Duration::from_secs(30);

async fn start(dir: &Path, name: &str) -> Node {
    std::fs::create_dir_all(dir).unwrap();
    let downloads = dir.join("downloads");
    Node::start(Options {
        secret: SecretKey::generate(),
        friends: JsonStore::load(dir.join("friends.json"), Vec::new()),
        invites: JsonStore::load(dir.join("invites.json"), Vec::new()),
        messages_dir: dir.join("messages"),
        download_dir: Arc::new(move || downloads.clone()),
        on_event: Arc::new(|_| {}),
        groups_dir: dir.join("groups"),
        nickname: name.to_string(),
        relay: None,
        lan: None,
    })
    .await
    .expect("node")
}

/// Where `n` answers on this computer.
fn here(n: &Node) -> SocketAddr {
    let (_, port) = n.nearby_hint().expect("port");
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
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

fn check(what: &str, ok: bool) -> bool {
    println!("  {}  {what}", if ok { "ok" } else { "ÉCHEC" });
    ok
}

#[tokio::main]
async fn main() {
    let work = PathBuf::from(std::env::args().nth(1).expect("usage: invite_test <workdir>"));
    let _ = std::fs::remove_dir_all(&work);
    // No relay nor directory: only the addresses given below.
    std::env::set_var("P2PSHARE_NO_INTERNET", "1");
    let a = start(&work.join("alice"), "Alice").await;
    let b = start(&work.join("bob"), "Bob").await;
    let c = start(&work.join("carol"), "Carol").await;
    let (ida, idb, idc) = (a.id(), b.id(), c.id());
    let mut ok = true;

    println!("1. Alice invite Bob");
    let link = invite::make(&ida, &a.create_invite(), "Alice");
    println!("     lien : {link}");
    let read = invite::parse(&link).expect("lien lisible");
    ok &= check("le lien donne l'ID et le nom d'Alice", read.id == ida && read.name == "Alice");
    let friends = b.accept_invite(&read.id, &read.name, &read.code).expect("accepté");
    ok &= check(
        "Bob a Alice dans ses amis, invitation en attente",
        friends.iter().any(|f| f.id == ida && f.name == "Alice" && f.invite == Some("pending")),
    );
    ok &= check("Alice n'a encore personne", a.friends().is_empty());
    b.nearby_peer(&ida, here(&a));
    ok &= wait("Alice et Bob amis et connectés", || {
        a.friends().iter().any(|f| f.id == idb && f.online)
            && b.friends().iter().any(|f| f.id == ida && f.online && f.invite.is_none())
    })
    .await;
    ok &= check(
        "Alice voit Bob sous son pseudo",
        a.friends().iter().any(|f| f.id == idb && f.name == "Bob"),
    );

    println!("2. Carol essaie le même lien");
    c.accept_invite(&read.id, &read.name, &read.code).expect("accepté");
    c.nearby_peer(&ida, here(&a));
    ok &= wait("Carol voit que le lien ne marche plus", || {
        c.friends().iter().any(|f| f.id == ida && f.invite == Some("refused"))
    })
    .await;
    ok &= check("Alice n'a pas Carol", !a.friends().iter().any(|f| f.id == idc));

    for n in [&a, &b, &c] {
        n.shutdown().await;
    }
    println!("{}", if ok { "Tout est bon." } else { "Des étapes ont échoué." });
    std::process::exit(if ok { 0 } else { 1 });
}
