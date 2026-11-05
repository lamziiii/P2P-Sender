//! End-to-end checks of delivery with two real nodes in one process: what
//! waits for an offline friend, and transfers cut by a disconnection.
//!
//!   cargo run --release --example scenarios -- <workdir>
//!   cargo run --release --example scenarios -- <workdir> features
//!
//! The second only checks, quickly, replies, reactions, read receipts,
//! typing, folders, pausing by hand and the speed limit.
//!
//! Offline periods last long enough for the sender to dial less often, so
//! the friend coming back is what reconnects them.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::SecretKey;
use p2pshare_lib::p2p::{Dir, Event, MessageStatus, Node, Options, Status, TransferView};
use p2pshare_lib::store::JsonStore;

const TIMEOUT: Duration = Duration::from_secs(120);
/// Long enough for quick retries to be over and the dial delay to grow.
const OFFLINE: Duration = Duration::from_secs(50);

struct Peer {
    name: &'static str,
    secret: SecretKey,
    dir: PathBuf,
    /// Who told us they are typing.
    typing: Arc<Mutex<Vec<String>>>,
}

impl Peer {
    fn new(name: &'static str, work: &Path) -> Self {
        Peer {
            name,
            secret: SecretKey::generate(),
            dir: work.join(name),
            typing: Arc::default(),
        }
    }

    fn id(&self) -> String {
        self.secret.public().to_string()
    }

    async fn start(&self) -> Node {
        std::fs::create_dir_all(&self.dir).unwrap();
        let downloads = self.dir.join("downloads");
        Node::start(Options {
            kept: JsonStore::read_only(Default::default(), Vec::new()),
            secret: self.secret.clone(),
            friends: JsonStore::load(self.dir.join("friends.json"), Vec::new()),
            invites: JsonStore::load(self.dir.join("invites.json"), Vec::new()),
            messages_dir: self.dir.join("messages"),
            download_dir: Arc::new(move || downloads.clone()),
            on_event: {
                let typing = self.typing.clone();
                Arc::new(move |e| {
                    if let Event::Typing { friend_id } = e {
                        typing.lock().unwrap().push(friend_id);
                    }
                })
            },
            groups_dir: self.dir.join("groups"),
            nickname: self.name.to_string(),
            relay: None,
            lan: None,
        })
        .await
        .expect("bind")
    }
}

async fn wait(what: &str, mut ok: impl FnMut() -> bool) -> f64 {
    let start = Instant::now();
    while !ok() {
        if start.elapsed() > TIMEOUT {
            panic!("ÉCHEC : {what} (après {}s)", TIMEOUT.as_secs());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let secs = start.elapsed().as_secs_f64();
    println!("  ok  {what} ({secs:.1}s)");
    secs
}

fn online(n: &Node, id: &str) -> bool {
    n.friends().iter().any(|f| f.id == id && f.online)
}

fn transfer(n: &Node, f: impl Fn(&TransferView) -> bool) -> Option<TransferView> {
    n.transfers().into_iter().find(f)
}

/// Accept every offer waiting on `n`.
fn accept_all(n: &Node) {
    for t in n.transfers() {
        if t.dir == Dir::In && t.status == Status::Pending {
            n.respond_to_offer(&t.id, true);
        }
    }
}

fn data(len: usize, seed: u32) -> Vec<u8> {
    (0..len as u32)
        .map(|i| (i.wrapping_add(seed).wrapping_mul(2654435761) >> 13) as u8)
        .collect()
}

#[tokio::main]
async fn main() {
    let work = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: scenarios <workdir>"),
    );
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let started = Instant::now();
    let (pa, pb) = (Peer::new("alice", &work), Peer::new("bob", &work));
    let (a, b) = (pa.start().await, pb.start().await);
    a.add_friend(&pb.id(), "Bob").unwrap();
    b.add_friend(&pa.id(), "Alice").unwrap();
    wait("A et B connectés", || online(&a, &pb.id()) && online(&b, &pa.id())).await;
    if std::env::args().nth(2).as_deref() == Some("features") {
        features(&work, (&pa, &a), (&pb, &b)).await;
        for n in [&a, &b] {
            n.shutdown().await;
        }
        println!(
            "\nTOUS LES SCÉNARIOS PASSENT en {:.1}s",
            started.elapsed().as_secs_f64()
        );
        return;
    }

    println!("1. Message et fichier pour un ami hors ligne");
    b.shutdown().await;
    wait("A voit B hors ligne", || !online(&a, &pb.id())).await;
    let file = work.join("photo.bin");
    std::fs::write(&file, data(300_000, 1)).unwrap();
    a.send_message(&pb.id(), "pendant ton absence", None).unwrap();
    // The pending message must come back from A's chat file.
    a.shutdown().await;
    let a = pa.start().await;
    assert!(a.messages(&pb.id()).iter().any(|m| m.text == "pendant ton absence"));
    println!("  ok  A redémarré, message en attente conservé");
    assert_eq!(a.send_files(&pb.id(), &[file.clone()]), 1);
    println!("     B hors ligne {}s…", OFFLINE.as_secs());
    tokio::time::sleep(OFFLINE).await;
    let back = Instant::now();
    let b = pb.start().await;
    wait("B revenu, connecté à A", || online(&a, &pb.id()) && online(&b, &pa.id())).await;
    let connected = back.elapsed().as_secs_f64();
    wait("B reçoit le message", || {
        b.messages(&pa.id()).iter().any(|m| m.text == "pendant ton absence")
    })
    .await;
    wait("A le voit remis", || {
        a.messages(&pb.id())
            .iter()
            .any(|m| m.text == "pendant ton absence" && m.status == Some(MessageStatus::Delivered))
    })
    .await;
    wait("B reçoit l'offre", || {
        transfer(&b, |t| t.dir == Dir::In && t.status == Status::Pending).is_some()
    })
    .await;
    println!(
        "     connecté {connected:.1}s après le retour de B, tout reçu {:.1}s après",
        back.elapsed().as_secs_f64()
    );
    accept_all(&b);
    wait("le fichier arrive", || {
        transfer(&a, |t| t.dir == Dir::Out && t.status == Status::Completed).is_some()
    })
    .await;

    println!("2. Quarante fichiers d'un coup");
    let files: Vec<PathBuf> = (0..40)
        .map(|i| {
            let p = work.join(format!("f{i:02}.bin"));
            std::fs::write(&p, data(64 * 1024 + i * 1000, i as u32)).unwrap();
            p
        })
        .collect();
    assert_eq!(a.send_files(&pb.id(), &files), 40);
    wait("les 40 offres arrivent et les 40 fichiers sont reçus", || {
        accept_all(&b);
        let done = b
            .transfers()
            .iter()
            .filter(|t| t.dir == Dir::In && t.status == Status::Completed && t.file_name.starts_with('f'))
            .count();
        done == 40
    })
    .await;
    for (i, f) in files.iter().enumerate() {
        let name = f.file_name().unwrap();
        let got = std::fs::read(pb.dir.join("downloads").join(name)).unwrap();
        assert_eq!(got, data(64 * 1024 + i * 1000, i as u32), "contenu de {name:?}");
    }
    println!("  ok  contenus identiques");

    println!("3. Transfert coupé par une déconnexion, puis repris");
    let big = work.join("gros.bin");
    let big_data = data(400 << 20, 7);
    std::fs::write(&big, &big_data).unwrap();
    a.send_files(&pb.id(), &[big.clone()]);
    let incoming = |n: &Node| transfer(n, |t| t.dir == Dir::In && t.file_name == "gros.bin");
    wait("B reçoit l'offre", || incoming(&b).is_some()).await;
    accept_all(&b);
    wait("le transfert avance", || {
        incoming(&b).is_some_and(|t| t.progress >= 5)
    })
    .await;
    a.suspend();
    wait("B voit A hors ligne (mise en veille)", || !online(&b, &pa.id())).await;
    let cut = incoming(&b).unwrap();
    println!("     coupé à {}% ({:?})", cut.progress, cut.status);
    tokio::time::sleep(Duration::from_secs(3)).await;
    a.resume();
    wait("reconnectés après la reprise", || online(&b, &pa.id())).await;
    wait("le transfert se termine", || {
        incoming(&b).is_some_and(|t| t.status == Status::Completed)
    })
    .await;
    let saved = incoming(&b).unwrap().saved_path.unwrap();
    assert!(std::fs::read(saved).unwrap() == big_data, "contenu repris");
    println!("  ok  contenu identique");

    println!("4. Annulation faite hors ligne");
    let big2 = work.join("gros2.bin");
    std::fs::write(&big2, &big_data[..200 << 20]).unwrap();
    a.send_files(&pb.id(), &[big2.clone()]);
    let incoming2 = |n: &Node| transfer(n, |t| t.dir == Dir::In && t.file_name == "gros2.bin");
    wait("B reçoit l'offre", || incoming2(&b).is_some()).await;
    accept_all(&b);
    wait("le transfert avance", || {
        incoming2(&b).is_some_and(|t| t.bytes > 0)
    })
    .await;
    a.suspend();
    wait("B voit A hors ligne", || !online(&b, &pa.id())).await;
    let out = transfer(&a, |t| t.dir == Dir::Out && t.file_name == "gros2.bin").unwrap();
    a.cancel_transfer(&out.id);
    a.network_changed();
    a.set_economy(true);
    tokio::time::sleep(Duration::from_secs(2)).await;
    a.resume();
    a.set_economy(false);
    wait("B apprend l'annulation", || {
        incoming2(&b).is_some_and(|t| t.status == Status::Cancelled)
    })
    .await;

    println!("5. Message de groupe pour un membre hors ligne");
    let gid = a.create_group("Projet", &[pb.id()]).unwrap();
    wait("B reçoit l'invitation", || b.groups().invites.iter().any(|i| i.gid == gid)).await;
    b.respond_to_invite(&gid, true);
    wait("B est membre", || {
        b.group_detail(&gid).is_some_and(|d| !d.syncing && d.members.len() == 2)
    })
    .await;
    b.shutdown().await;
    wait("A voit B hors ligne", || !online(&a, &pb.id())).await;
    a.group_send(&gid, "message de groupe").unwrap();
    println!("     B hors ligne {}s…", OFFLINE.as_secs());
    tokio::time::sleep(OFFLINE).await;
    let back = Instant::now();
    let b = pb.start().await;
    wait("B revenu reçoit le message de groupe", || {
        b.group_messages(&gid).iter().any(|m| m.text == "message de groupe")
    })
    .await;
    println!("     {:.1}s après le retour de B", back.elapsed().as_secs_f64());

    for n in [&a, &b] {
        n.shutdown().await;
    }
    println!(
        "\nTOUS LES SCÉNARIOS PASSENT en {:.1}s",
        started.elapsed().as_secs_f64()
    );
}

async fn features(work: &Path, (pa, a): (&Peer, &Node), (pb, b): (&Peer, &Node)) {
    let (ida, idb) = (pa.id(), pb.id());
    println!("A. Réponse, réaction, vu, en train d'écrire");
    let hello = a.send_message(&idb, "bonjour", None).unwrap();
    wait("B reçoit le message", || b.messages(&ida).iter().any(|m| m.id == hello.id)).await;
    let answer = b.send_message(&ida, "salut", Some(&hello.id)).unwrap();
    assert_eq!(answer.reply.as_ref().unwrap().text, "bonjour");
    wait("A reçoit la réponse avec sa citation", || {
        a.messages(&idb).iter().any(|m| {
            m.id == answer.id && m.reply.as_ref().is_some_and(|r| r.id == hello.id && r.from == ida)
        })
    })
    .await;
    let reaction = |n: &Node, peer: &str, id: &str, who: &str| {
        n.messages(peer)
            .iter()
            .find(|m| m.id == id)
            .and_then(|m| m.reactions.get(who).cloned())
    };
    assert!(b.react(&ida, &hello.id, Some("🤡")).is_err());
    b.react(&ida, &hello.id, Some("👍")).unwrap();
    wait("A voit la réaction de B", || {
        reaction(a, &idb, &hello.id, &idb).as_deref() == Some("👍")
    })
    .await;
    // The same again takes it back.
    b.react(&ida, &hello.id, Some("👍")).unwrap();
    wait("A la voit retirée", || reaction(a, &idb, &hello.id, &idb).is_none()).await;
    b.react(&ida, &hello.id, Some("❤️")).unwrap();
    wait("A voit le cœur", || {
        reaction(a, &idb, &hello.id, &idb).as_deref() == Some("❤️")
    })
    .await;
    let status = |id: &str| a.messages(&idb).iter().find(|m| m.id == id).and_then(|m| m.status);
    b.mark_chat_read(&ida);
    wait("A voit son message lu", || status(&hello.id) == Some(MessageStatus::Read)).await;
    a.chat_typing(&idb);
    wait("B voit A écrire", || pb.typing.lock().unwrap().contains(&ida)).await;
    // Off on one side: nothing more either way.
    b.set_read_receipts(false);
    let second = a.send_message(&idb, "encore", None).unwrap();
    wait("B le reçoit", || b.messages(&ida).iter().any(|m| m.id == second.id)).await;
    b.mark_chat_read(&ida);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(status(&second.id), Some(MessageStatus::Delivered));
    b.set_read_receipts(true);
    println!("  ok  pas de « vu » quand B l'a désactivé");

    println!("B. Dossier");
    let src = work.join("Photos");
    std::fs::create_dir_all(src.join("2024/juillet")).unwrap();
    std::fs::create_dir_all(src.join("vide")).unwrap();
    let tree = [("a.bin", 1000), ("2024/b.bin", 200_000), ("2024/juillet/c.bin", 3)];
    for (i, (rel, len)) in tree.iter().enumerate() {
        std::fs::write(src.join(rel), data(*len, i as u32)).unwrap();
    }
    let folder_of = |n: &Node| {
        n.transfers()
            .into_iter()
            .filter(|t| t.dir == Dir::In && t.folder.is_some())
            .collect::<Vec<_>>()
    };
    assert_eq!(a.send_folder(&idb, &src).unwrap(), 3);
    wait("B reçoit les offres du dossier", || {
        folder_of(b).iter().filter(|t| t.status == Status::Pending).count() == 3
    })
    .await;
    let fid = folder_of(b)[0].folder.as_ref().unwrap().id.clone();
    assert_eq!(b.respond_to_folder(&ida, &fid, true), 3);
    wait("le dossier arrive", || {
        folder_of(b).iter().filter(|t| t.status == Status::Completed).count() == 3
    })
    .await;
    let downloads = pb.dir.join("downloads");
    for (i, (rel, len)) in tree.iter().enumerate() {
        let got = std::fs::read(downloads.join("Photos").join(rel)).unwrap();
        assert_eq!(got, data(*len, i as u32), "{rel}");
    }
    println!("  ok  arborescence et contenus identiques");
    // From a trusted friend: no question, and a folder name of its own.
    b.set_friend_auto_accept(&ida, true);
    assert!(b.friends().iter().any(|f| f.id == ida && f.auto_accept));
    a.send_folder(&idb, &src).unwrap();
    wait("le second arrive tout seul dans « Photos (2) »", || {
        downloads.join("Photos (2)/2024/juillet/c.bin").is_file()
            && folder_of(b).iter().filter(|t| t.status == Status::Completed).count() == 6
    })
    .await;
    b.set_friend_auto_accept(&ida, false);

    println!("C. Pause à la main et limite de vitesse");
    let limit = 4 << 20;
    a.set_speed_limit(Some(limit));
    let big = work.join("limite.bin");
    let big_data = data(24 << 20, 9);
    std::fs::write(&big, &big_data).unwrap();
    a.send_files(&idb, &[big.clone()]);
    let incoming = |n: &Node| transfer(n, |t| t.dir == Dir::In && t.file_name == "limite.bin");
    let outgoing = |n: &Node| transfer(n, |t| t.dir == Dir::Out && t.file_name == "limite.bin");
    wait("B reçoit l'offre", || incoming(b).is_some()).await;
    accept_all(b);
    wait("le transfert avance", || incoming(b).is_some_and(|t| t.bytes > (2 << 20))).await;
    let tid = incoming(b).unwrap().id;
    assert!(b.pause_transfer(&tid));
    wait("A voit la pause de B", || {
        outgoing(a).is_some_and(|t| t.status == Status::Paused && t.paused_by == Some("peer"))
    })
    .await;
    let held = incoming(b).unwrap().bytes;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(incoming(b).unwrap().bytes, held, "rien ne passe en pause");
    println!("  ok  plus rien ne passe");
    assert!(b.resume_transfer(&tid));
    wait("reprise", || incoming(b).is_some_and(|t| t.bytes > held)).await;
    let out = outgoing(a).unwrap().id;
    assert!(a.pause_transfer(&out));
    wait("B voit la pause de A", || {
        incoming(b).is_some_and(|t| t.status == Status::Paused && t.paused_by == Some("peer"))
    })
    .await;
    assert!(a.resume_transfer(&out));
    let resumed = Instant::now();
    let from = incoming(b).unwrap().bytes;
    wait("le transfert se termine", || {
        incoming(b).is_some_and(|t| t.status == Status::Completed)
    })
    .await;
    let speed = (big_data.len() as u64 - from) as f64 / resumed.elapsed().as_secs_f64();
    println!(
        "     {:.1} Mo/s pour une limite de {} Mo/s",
        speed / 1048576.0,
        limit >> 20
    );
    assert!(speed < limit as f64 * 1.3, "limite dépassée");
    let saved = incoming(b).unwrap().saved_path.unwrap();
    assert!(std::fs::read(saved).unwrap() == big_data, "contenu repris");
    println!("  ok  contenu identique");
    a.set_speed_limit(None);
}
