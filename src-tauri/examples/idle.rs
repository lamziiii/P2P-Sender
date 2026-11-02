//! An idle node, to measure what the P2P core costs when nothing happens.
//!
//!   idle <workdir> <a|b> <seconds>
//!
//! A has B and five friends that never come online; B has A. Each side
//! publishes its ID in <workdir>/<role>.id and waits for the other's. Both
//! run for <seconds> then exit; measure the process from outside, e.g.
//! `(Get-Process -Id <pid>).TotalProcessorTime` in PowerShell.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::SecretKey;
use p2pshare_lib::p2p::{Event, Node, Options};
use p2pshare_lib::store::JsonStore;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (work, role, secs) = match (args.first(), args.get(1), args.get(2)) {
        (Some(w), Some(r), Some(s)) if r == "a" || r == "b" => {
            (PathBuf::from(w), r.clone(), s.parse::<u64>().expect("seconds"))
        }
        _ => {
            eprintln!("usage: idle <workdir> <a|b> <seconds>");
            std::process::exit(2);
        }
    };
    std::fs::create_dir_all(&work).unwrap();
    let dir = work.join(&role);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let events = Arc::new(AtomicU64::new(0));
    let counted = events.clone();
    let downloads = dir.join("downloads");
    let node = Node::start(Options {
        secret: SecretKey::generate(),
        friends: JsonStore::load(dir.join("friends.json"), Vec::new()),
        messages_dir: dir.join("messages"),
        download_dir: Arc::new(move || downloads.clone()),
        on_event: Arc::new(move |e| {
            counted.fetch_add(1, Ordering::Relaxed);
            if let Event::FriendStatus { friend_id, online } = e {
                println!("[{}] {} online={online}", now(), &friend_id[..8]);
            }
        }),
        groups_dir: dir.join("groups"),
        nickname: role.clone(),
        relay: None,
        lan: None,
    })
    .await
    .expect("bind");
    std::fs::write(work.join(format!("{role}.id")), node.id()).unwrap();
    println!("pid {} id {}", std::process::id(), node.id());

    if role == "a" {
        for i in 0..5 {
            let id = SecretKey::generate().public().to_string();
            node.add_friend(&id, &format!("offline {i}")).unwrap();
        }
    }
    let other = work.join(if role == "a" { "b.id" } else { "a.id" });
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        if let Ok(id) = std::fs::read_to_string(&other) {
            if id.trim().len() == 64 {
                node.add_friend(id.trim(), "peer").unwrap();
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_secs(secs)).await;
    println!("events {}", events.load(Ordering::Relaxed));
    node.shutdown().await;
}

fn now() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    format!("{:.1}", t % 1000.0)
}
