//! Peer aliases: other names the owner gives peers, sharing one namespace
//! with peers' names.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

mod common;

use common::{Clock, Daemon, ask, kind, name, peer, scope, settle, signed, url};
use cp_api::{
    Aliases, Approval, ErrorKind, ListThreadsQuery, MAX_ALIASES, NewThread, Peer, PeerInput,
    WebhookCreate, WebhookEvents, WebhookOrigin, WebhookType, WebhookUpdate, WebhookUrl,
};
use cp_thread::{Body, Name, Title};

fn aliases(names: &[&str]) -> Aliases {
    Aliases::try_from(names.iter().map(|n| name(n)).collect::<Vec<_>>()).unwrap()
}

fn set(d: &mut Daemon, peer: &str, names: &[&str]) -> Result<Peer, cp_api::Error> {
    d.call(|c| c.set_peer_aliases(&name(peer), &aliases(names)))
}

fn names(list: &[Name]) -> Vec<&str> {
    list.iter().map(Name::as_str).collect()
}

/// Alice peered with bob and carol.
fn three(clock: &Clock) -> (Daemon, Daemon, Daemon) {
    let mut alice = Daemon::new("alice", clock, 1);
    let mut bob = Daemon::new("bob", clock, 2);
    let mut carol = Daemon::new("carol", clock, 3);
    peer(&mut bob, &mut alice);
    peer(&mut carol, &mut alice);
    (alice, bob, carol)
}

#[test]
fn aliases_are_sorted_once_and_bounded() {
    assert_eq!(
        names(aliases(&["bobby", "b", "bobby"]).as_slice()),
        ["b", "bobby"]
    );
    let many: Vec<Name> = (0..=MAX_ALIASES).map(|i| name(&format!("a{i}"))).collect();
    assert!(Aliases::try_from(many).is_err());
    assert!(serde_json::from_str::<cp_api::PeerAliases>(r#"{"aliases":["Not A Name"]}"#).is_err());
}

#[test]
fn aliases_share_the_namespace_of_peers_names() {
    let clock = Clock::new();
    let (mut alice, _bob, _carol) = three(&clock);

    let p = set(&mut alice, "bob", &["bobby", "b"]).unwrap();
    assert_eq!(
        (p.name.as_str(), names(&p.aliases)),
        ("bob", vec!["b", "bobby"])
    );
    // Replaced whole, and the peer may be named by an alias.
    let p = set(&mut alice, "bobby", &["bobby", "robert"]).unwrap();
    assert_eq!(names(&p.aliases), ["bobby", "robert"]);
    assert_eq!(
        names(&alice.call(|c| c.peers(None)).unwrap().peers[0].aliases),
        ["bobby", "robert"]
    );

    for (alias, says) in [
        ("alice", "owner"),
        ("bob", "already this peer's name"),
        ("carol", "another peer's name"),
    ] {
        let e = set(&mut alice, "bob", &[alias]).unwrap_err();
        assert!(
            e.kind == ErrorKind::Conflict && e.detail.contains(says),
            "{alias}: {e:?}"
        );
    }
    let e = set(&mut alice, "carol", &["robert"]).unwrap_err();
    assert!(
        e.kind == ErrorKind::Conflict && e.detail.contains("robert is an alias of bob"),
        "{e:?}"
    );
    assert_eq!(kind(&set(&mut alice, "dave", &["d"])), ErrorKind::NotFound);
    // A refusal changes nothing.
    assert_eq!(
        names(&alice.call(|c| c.peer(&name("bob"), None)).unwrap().aliases),
        ["bobby", "robert"]
    );
}

#[test]
fn a_peer_cannot_be_added_or_approved_under_an_alias() {
    let clock = Clock::new();
    let (mut alice, _bob, _carol) = three(&clock);
    set(&mut alice, "bob", &["bobby", "dave"]).unwrap();

    let e = alice
        .call(|c| {
            c.add_peer(PeerInput {
                name: name("bobby"),
                url: url("https://elsewhere.test"),
                note: None,
            })
        })
        .unwrap_err();
    assert!(
        e.kind == ErrorKind::Conflict && e.detail.contains("bobby is an alias of bob"),
        "{e:?}"
    );

    // Dave asks to be called dave, which is an alias here.
    let mut dave = Daemon::new("dave", &clock, 4);
    ask(&mut dave, &mut alice);
    let id = alice.call(|c| c.requests()).unwrap().requests[0].id.clone();
    let approve = |alice: &mut Daemon, as_name: Option<&str>| {
        alice.call(|c| {
            c.begin_approval(
                &id,
                Approval {
                    name: as_name.map(name),
                },
            )
        })
    };
    let e = approve(&mut alice, None).unwrap_err();
    assert!(
        e.kind == ErrorKind::Conflict && e.detail.contains("dave is an alias of bob"),
        "{e:?}"
    );
    assert_eq!(
        kind(&approve(&mut alice, Some("bobby"))),
        ErrorKind::Conflict
    );
    let pending = approve(&mut alice, Some("david")).unwrap();
    let accepted = signed(&pending.call.headers, pending.call.path, &pending.call.body);
    dave.call(|c| {
        c.authenticate_peer(&accepted)?;
        c.accepted(&accepted.key)
    })
    .unwrap();
    let p = alice.call(|c| c.commit_approval(&pending)).unwrap();
    assert_eq!((p.name.as_str(), p.aliases.len()), ("david", 0));
}

#[test]
fn an_alias_works_wherever_a_peer_is_named() {
    let clock = Clock::new();
    let (mut alice, mut bob, _carol) = three(&clock);
    set(&mut alice, "bob", &["bobby"]).unwrap();

    assert_eq!(
        alice
            .call(|c| c.peer(&name("bobby"), None))
            .unwrap()
            .name
            .as_str(),
        "bob"
    );
    let open = |alice: &mut Daemon, to: &str, within: Option<&cp_api::Peers>| {
        alice.call(|c| {
            c.open_thread(
                NewThread {
                    to: name(to),
                    title: Title::try_from("via an alias").unwrap(),
                    kind: None,
                    labels: Vec::new(),
                    body: Body::default(),
                },
                within,
            )
        })
    };
    let t = open(&mut alice, "bobby", None).unwrap();
    assert_eq!(
        (t.peer.as_str(), t.recipient.as_str()),
        ("bob", "bob"),
        "stored under the peer's name"
    );
    settle(&mut alice, &mut bob);
    assert_eq!(bob.summary(t.id.as_str()).peer.as_str(), "alice");

    let listed = |alice: &mut Daemon, peer: &str, within: Option<&cp_api::Peers>| {
        let q = ListThreadsQuery {
            peer: Some(name(peer)),
            ..ListThreadsQuery::default()
        };
        alice.call(|c| c.threads(&q, within)).unwrap().len()
    };
    assert_eq!(listed(&mut alice, "bobby", None), 1);

    // Lists of peers keep names: an alias and its peer count once.
    let token = alice
        .call(|c| {
            c.create_agent_token(cp_api::AgentTokenInput {
                name: name("runner"),
                peers: scope(&["bobby", "bob"]),
                expires_at: None,
            })
        })
        .unwrap();
    assert_eq!(token.peers, scope(&["bob"]));
    let hook = alice
        .call(|c| {
            c.create_webhook(WebhookCreate {
                name: name("chat"),
                kind: WebhookType::Generic,
                url: WebhookUrl::new("https://hooks.example.com/x".into()).unwrap(),
                events: WebhookEvents::All,
                origin: WebhookOrigin::Both,
                enabled: true,
                peers: scope(&["bobby"]),
                secret: None,
                headers: Vec::new(),
            })
        })
        .unwrap();
    assert_eq!(hook.peers, scope(&["bob"]));
    let hook = alice
        .call(|c| {
            c.update_webhook(
                &name("chat"),
                WebhookUpdate {
                    peers: Some(scope(&["bobby", "carol"])),
                    ..WebhookUpdate::default()
                },
            )
        })
        .unwrap();
    assert_eq!(hook.peers, scope(&["bob", "carol"]));

    // A caller limited to carol cannot reach bob by his alias, nor tell it
    // from no peer at all.
    let carol_only = scope(&["carol"]);
    let e = alice
        .call(|c| c.peer(&name("bobby"), Some(&carol_only)))
        .unwrap_err();
    assert_eq!(
        (e.kind, e.detail.as_str()),
        (ErrorKind::NotFound, "bobby is not a peer")
    );
    let e = open(&mut alice, "bobby", Some(&carol_only)).unwrap_err();
    assert_eq!(
        (e.kind, e.detail.as_str()),
        (ErrorKind::NotFound, "bobby is not a peer")
    );
    assert_eq!(listed(&mut alice, "bobby", Some(&carol_only)), 0);
    let visible = alice.call(|c| c.peers(Some(&carol_only))).unwrap().peers;
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].name.as_str(), "carol");
}

#[test]
fn aliases_go_with_their_peer() {
    let clock = Clock::new();
    let (mut alice, mut bob, _carol) = three(&clock);
    set(&mut alice, "bob", &["bobby"]).unwrap();
    set(&mut alice, "carol", &["c"]).unwrap();
    alice.send("bob", "kept after removal");
    settle(&mut alice, &mut bob);

    alice.call(|c| c.remove_peer(&name("bobby"))).unwrap();
    assert_eq!(
        kind(&alice.call(|c| c.peer(&name("bob"), None))),
        ErrorKind::NotFound
    );
    assert_eq!(
        kind(&alice.call(|c| c.peer(&name("bobby"), None))),
        ErrorKind::NotFound
    );
    // The alias is free again, but bob's name stays with bob's URL.
    let p = set(&mut alice, "carol", &["bobby", "c"]).unwrap();
    assert_eq!(names(&p.aliases), ["bobby", "c"]);
    let e = set(&mut alice, "carol", &["bob"]).unwrap_err();
    assert!(
        e.kind == ErrorKind::Conflict && e.detail.contains("was a peer at https://bob.test"),
        "{e:?}"
    );
    let q = ListThreadsQuery {
        peer: Some(name("bob")),
        ..ListThreadsQuery::default()
    };
    assert_eq!(alice.call(|c| c.threads(&q, None)).unwrap().len(), 1);
}

#[test]
fn reapproving_a_peer_keeps_its_aliases() {
    let clock = Clock::new();
    let mut alice = Daemon::new("alice", &clock, 1);
    let mut bob = Daemon::new("bob", &clock, 2);
    peer(&mut bob, &mut alice);
    set(&mut alice, "bob", &["bobby"]).unwrap();
    // Bob's daemon starts over with a new key and asks again from the same
    // URL: approved under the same name, it is the same peer to alice.
    let mut bob = Daemon::new("bob", &clock, 5);
    peer(&mut bob, &mut alice);
    assert_eq!(
        names(&alice.call(|c| c.peer(&name("bob"), None)).unwrap().aliases),
        ["bobby"]
    );
}
