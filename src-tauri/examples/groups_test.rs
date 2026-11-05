//! End-to-end test of groups with three real nodes in one process.
//!
//!   cargo run --release --example groups_test -- <workdir>
//!
//! A is friends with B and C; B and C are not friends with each other.
//! Set P2P_RELAY_URL (and P2P_RELAY_TOKEN) to go through a self-hosted relay.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::SecretKey;
use p2pshare_lib::p2p::{CustomRelay, Node, Options};
use p2pshare_lib::store::JsonStore;

const TIMEOUT: Duration = Duration::from_secs(90);

struct Peer {
    name: &'static str,
    secret: SecretKey,
    dir: PathBuf,
}

impl Peer {
    fn new(name: &'static str, work: &Path) -> Self {
        Peer {
            name,
            secret: SecretKey::generate(),
            dir: work.join(name),
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
            on_event: Arc::new(|_| {}),
            groups_dir: self.dir.join("groups"),
            nickname: self.name.to_string(),
            relay: relay(),
            lan: None,
        })
        .await
        .expect("bind")
    }
}

/// P2P_RELAY_URL / P2P_RELAY_TOKEN: run the test through a self-hosted relay.
fn relay() -> Option<CustomRelay> {
    let url = std::env::var("P2P_RELAY_URL").ok()?;
    let token = std::env::var("P2P_RELAY_TOKEN").unwrap_or_default();
    Some(CustomRelay::parse(&url, &token).expect("P2P_RELAY_URL"))
}

async fn wait(what: &str, mut ok: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ok() {
        if start.elapsed() > TIMEOUT {
            panic!("ÉCHEC : {what} (après {}s)", TIMEOUT.as_secs());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    println!("  ok  {what} ({:.1}s)", start.elapsed().as_secs_f64());
}

fn has_message(node: &Node, gid: &str, text: &str) -> bool {
    node.group_messages(gid).iter().any(|m| m.text == text)
}

#[tokio::main]
async fn main() {
    let work = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: groups_test <workdir>"),
    );
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let started = Instant::now();

    let (pa, pb, pc) = (
        Peer::new("alice", &work),
        Peer::new("bob", &work),
        Peer::new("carol", &work),
    );
    let (a, b, c) = (pa.start().await, pb.start().await, pc.start().await);
    a.add_friend(&pb.id(), "Bob").unwrap();
    a.add_friend(&pc.id(), "Carol").unwrap();
    b.add_friend(&pa.id(), "Alice").unwrap();
    c.add_friend(&pa.id(), "Alice").unwrap();

    println!("1. Invitation");
    let mut last = Instant::now();
    wait("A connectée à B et C", || {
        if last.elapsed() > Duration::from_secs(5) {
            last = Instant::now();
            let st = |n: &Node| {
                n.friends()
                    .iter()
                    .map(|f| format!("{}={}", f.name, f.online))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            println!("     A[{}] B[{}] C[{}]", st(&a), st(&b), st(&c));
        }
        a.friends().iter().all(|f| f.online)
    })
    .await;
    let gid = a.create_group("Projet", &[pb.id(), pc.id()]).unwrap();
    wait("B et C reçoivent l'invitation", || {
        b.groups().invites.iter().any(|i| i.gid == gid)
            && c.groups().invites.iter().any(|i| i.gid == gid)
    })
    .await;
    b.respond_to_invite(&gid, true);
    c.respond_to_invite(&gid, true);
    wait("B et C sont membres", || {
        let ok = |n: &Node| {
            n.group_detail(&gid)
                .is_some_and(|d| !d.syncing && d.members.len() == 3)
        };
        ok(&b) && ok(&c)
    })
    .await;
    wait("B et C (non amis) se connectent directement", || {
        b.group_detail(&gid)
            .is_some_and(|d| d.members.iter().all(|m| m.online))
    })
    .await;
    wait("les pseudos sont partagés", || {
        b.group_detail(&gid)
            // Not B's friend: her nickname comes with the start of her id.
            .is_some_and(|d| d.members.iter().any(|m| m.name.starts_with("carol (")))
    })
    .await;

    println!("2. Discussion");
    b.group_send(&gid, "salut tout le monde").unwrap();
    wait("A et C reçoivent le message de B", || {
        has_message(&a, &gid, "salut tout le monde") && has_message(&c, &gid, "salut tout le monde")
    })
    .await;
    wait("accusé « reçu par 2/2 » chez B", || {
        b.group_messages(&gid)
            .iter()
            .any(|m| m.mine && m.received_by == 2 && m.total == 2)
    })
    .await;

    let log = pc.dir.join("groups").join(format!("{gid}.jsonl"));
    wait("journal de C chiffré sur le disque", || {
        std::fs::read_to_string(&log)
            .is_ok_and(|t| t.lines().count() > 3 && t.lines().all(|l| l.starts_with("pv1:")))
    })
    .await;

    println!("2b. Mentions, épingles, sourdine, dossier déposé");
    assert!(b
        .group_detail(&gid)
        .is_some_and(|d| d.members.iter().any(|m| m.handle == "carol")));
    b.set_group_muted(&gid, true);
    assert!(b.groups().groups.iter().any(|g| g.id == gid && g.muted));
    c.group_send(&gid, "@Bob tu as vu ?").unwrap();
    wait("B voit qu'on le mentionne", || {
        b.group_messages(&gid)
            .iter()
            .any(|m| m.text == "@Bob tu as vu ?" && m.mentions_me)
    })
    .await;
    assert!(a.group_messages(&gid).iter().all(|m| !m.mentions_me));
    assert!(b
        .groups()
        .groups
        .iter()
        .any(|g| g.id == gid && g.unread_mentions == 1));
    let mid = a.group_messages(&gid)[0].id.clone();
    a.group_pin(&gid, &mid, true).unwrap();
    wait("C voit le message épinglé", || {
        c.group_detail(&gid).is_some_and(|d| d.pins.len() == 1 && d.pins[0].target == mid)
            && c.group_messages(&gid).iter().any(|m| m.id == mid && m.pinned)
    })
    .await;
    b.group_pin(&gid, &mid, false).unwrap();
    wait("A voit le message désépinglé", || {
        a.group_detail(&gid).is_some_and(|d| d.pins.is_empty())
    })
    .await;
    let folder = work.join("dossier");
    std::fs::create_dir_all(folder.join("sous")).unwrap();
    std::fs::write(folder.join("a.txt"), b"a").unwrap();
    std::fs::write(folder.join("sous").join("b.txt"), b"b").unwrap();
    assert_eq!(a.group_add_paths(&gid, vec![folder]).await, 2);
    wait("C voit les fichiers du dossier avec leur chemin", || {
        let files = c.group_files(&gid);
        files.len() == 2
            && files.iter().any(|f| f.dir.as_deref() == Some("dossier/sous") && f.name == "b.txt")
    })
    .await;
    let sub = c.group_files(&gid).into_iter().find(|f| f.name == "b.txt").unwrap();
    c.group_download(&gid, &sub.id).unwrap();
    wait("C range le fichier du dossier dans son dossier", || {
        c.group_file_path(&gid, &sub.id).is_some_and(|p| {
            p.ends_with(Path::new("P2PShare/Projet/dossier/sous/b.txt"))
                && std::fs::read(&p).is_ok_and(|d| d == b"b")
        })
    })
    .await;
    for f in a.group_files(&gid) {
        a.group_delete_file(&gid, &f.id);
    }
    wait("dépôt vidé", || c.group_files(&gid).is_empty()).await;

    println!("3. Relais vers un membre hors ligne");
    c.shutdown().await;
    b.group_send(&gid, "pendant ton absence").unwrap();
    wait("A reçoit le message", || {
        has_message(&a, &gid, "pendant ton absence")
    })
    .await;
    b.shutdown().await;
    let c = pc.start().await;
    wait(
        "C, revenue, le reçoit via A alors que B est hors ligne",
        || has_message(&c, &gid, "pendant ton absence"),
    )
    .await;

    println!("4. Dépôt de fichiers");
    let file = work.join("rapport.bin");
    let data: Vec<u8> = (0..5 * 1024 * 1024u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    std::fs::write(&file, &data).unwrap();
    assert_eq!(a.group_add_files(&gid, vec![file.clone()]).await, 1);
    wait("C voit le fichier dans le dépôt", || {
        c.group_files(&gid).len() == 1
    })
    .await;
    let fid = c.group_files(&gid)[0].id.clone();
    c.group_download(&gid, &fid).unwrap();
    wait("C l'a téléchargé depuis A", || {
        c.group_files(&gid).iter().any(|f| f.local)
    })
    .await;
    assert_eq!(
        std::fs::read(c.group_file_path(&gid, &fid).unwrap()).unwrap(),
        data
    );
    println!("  ok  contenu identique");
    a.shutdown().await;
    let b = pb.start().await;
    wait("B voit le fichier", || b.group_files(&gid).len() == 1).await;
    b.group_download(&gid, &fid).unwrap();
    wait("B le télécharge depuis C, A étant hors ligne", || {
        b.group_files(&gid).iter().any(|f| f.local)
    })
    .await;
    assert_eq!(
        std::fs::read(b.group_file_path(&gid, &fid).unwrap()).unwrap(),
        data
    );
    println!("  ok  contenu identique");

    println!("5. Retrait d'un membre");
    // Only the creator (A) or whoever added C (A too) may remove her.
    assert!(b
        .group_detail(&gid)
        .is_some_and(|d| d.members.iter().all(|m| !m.removable)));
    let a = pa.start().await;
    // The removed member learns it from the remover, connected to her.
    wait("A reconnectée à C", || {
        a.friends().iter().any(|f| f.id == pc.id() && f.online)
    })
    .await;
    a.group_remove_member(&gid, &pc.id());
    wait("C apprend qu'elle a été retirée", || {
        c.group_detail(&gid).is_some_and(|d| d.removed)
    })
    .await;
    assert!(c.group_send(&gid, "je suis encore là ?").is_err());
    wait("B voit le retrait", || {
        b.group_detail(&gid).is_some_and(|d| d.members.len() == 2)
    })
    .await;
    b.group_send(&gid, "entre nous").unwrap();
    wait("A reçoit le message suivant", || {
        has_message(&a, &gid, "entre nous")
    })
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        !has_message(&c, &gid, "entre nous"),
        "C ne doit plus rien recevoir"
    );
    println!("  ok  C ne reçoit plus les messages");

    for n in [&a, &b, &c] {
        n.shutdown().await;
    }
    println!(
        "\nTOUS LES TESTS PASSENT en {:.1}s",
        started.elapsed().as_secs_f64()
    );
}
