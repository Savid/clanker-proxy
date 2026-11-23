//! Peer aliases: other names the owner gives peers, sharing one namespace
//! with peers' names.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

mod common;

use common::{Clock, Daemon, ask, kind, name, offered, peer, scope, settle, signed, url};
use cp_api::{
    Aliases, Approval, ErrorKind, ListThreadsQuery, MAX_ALIASES, NewThread, Peer, PeerInput,
    PeeringRequest, WebhookCreate, WebhookEvents, WebhookOrigin, WebhookType, WebhookUpdate,
    WebhookUrl,
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

/// `asker` asks `approver` to peer, and is approved as `as_name`.
fn approve_as(asker: &mut Daemon, approver: &mut Daemon, as_name: &str) -> Peer {
    ask(asker, approver);
    approve_asked(asker, approver, as_name)
}

/// `approver` approves the request `asker` sent as `as_name`.
fn approve_asked(asker: &mut Daemon, approver: &mut Daemon, as_name: &str) -> Peer {
    let id = approver.call(|c| c.requests()).unwrap().requests[0]
        .id
        .clone();
    let pending = approver
        .call(|c| {
            c.begin_approval(
                &id,
                Approval {
                    name: Some(name(as_name)),
                },
            )
        })
        .unwrap();
    let accepted = signed(&pending.call.headers, pending.call.path, &pending.call.body);
    asker
        .call(|c| {
            c.authenticate_peer(&accepted)?;
            c.accepted(&accepted.key, &offered(&pending.call.body))
        })
        .unwrap();
    approver.call(|c| c.commit_approval(&pending)).unwrap()
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
    let mut many: Vec<Name> = (0..MAX_ALIASES).map(|i| name(&format!("a{i}"))).collect();
    many.push(name("a0"));
    assert_eq!(
        Aliases::try_from(many.clone()).unwrap().as_slice().len(),
        MAX_ALIASES,
        "a repeat counts once"
    );
    many.push(name("a10"));
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
        c.accepted(&accepted.key, &offered(&pending.call.body))
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

    // Removing takes the peer's name: an alias, perhaps meant for unalias,
    // removes nothing.
    let e = alice.call(|c| c.remove_peer(&name("bobby"))).unwrap_err();
    assert!(
        e.kind == ErrorKind::Conflict && e.detail.contains("bobby is an alias of bob"),
        "{e:?}"
    );
    alice.call(|c| c.peer(&name("bob"), None)).unwrap();
    alice.call(|c| c.remove_peer(&name("bob"))).unwrap();
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

#[test]
fn a_removed_peers_name_is_never_an_alias() {
    let clock = Clock::new();
    let (mut alice, _bob, _carol) = three(&clock);
    alice.send("bob", "kept after removal");
    alice.call(|c| c.remove_peer(&name("bob"))).unwrap();
    // Bob's daemon comes back from the same URL, approved as robert. His old
    // name still names the old threads, so it cannot become robert's alias.
    let mut bob = Daemon::new("bob", &clock, 5);
    approve_as(&mut bob, &mut alice, "robert");
    let e = set(&mut alice, "robert", &["bob"]).unwrap_err();
    assert!(
        e.kind == ErrorKind::Conflict && e.detail.contains("was a peer at https://bob.test"),
        "{e:?}"
    );
}

#[test]
fn a_peer_that_moved_leaves_its_old_name_to_its_threads() {
    let clock = Clock::new();
    let (mut alice, bob, _carol) = three(&clock);
    set(&mut alice, "bob", &["bobby"]).unwrap();
    let t = alice.send("bob", "queued for the old bob");

    // Bob's daemon, with the same key, asks again from a new URL, and alice
    // approves it as bob2: the old bob is forgotten as if removed.
    let moved = PeeringRequest {
        name: name("bob"),
        url: url("https://bob-moved.test"),
        key: bob.core.public_key().clone(),
        note: None,
        aliases: cp_api::Aliases::default(),
    };
    alice.call(|c| c.request_received(&moved)).unwrap();
    let id = alice.call(|c| c.requests()).unwrap().requests[0].id.clone();
    let pending = alice
        .call(|c| {
            c.begin_approval(
                &id,
                Approval {
                    name: Some(name("bob2")),
                },
            )
        })
        .unwrap();
    alice.call(|c| c.commit_approval(&pending)).unwrap();

    for gone in ["bob", "bobby"] {
        assert_eq!(
            kind(&alice.call(|c| c.peer(&name(gone), None))),
            ErrorKind::NotFound,
            "{gone}"
        );
    }
    let s = alice.summary(t.id.as_str());
    assert_eq!((s.peer.as_str(), s.undelivered, s.failed), ("bob", 0, 1));
    let e = set(&mut alice, "bob2", &["bob"]).unwrap_err();
    assert!(
        e.kind == ErrorKind::Conflict && e.detail.contains("was a peer at https://bob.test"),
        "{e:?}"
    );
    let e = alice
        .call(|c| {
            c.add_peer(PeerInput {
                name: name("bob"),
                url: url("https://elsewhere.test"),
                note: None,
            })
        })
        .unwrap_err();
    assert!(
        e.kind == ErrorKind::Conflict && e.detail.contains("was a peer at"),
        "{e:?}"
    );
    let q = ListThreadsQuery {
        peer: Some(name("bob")),
        ..ListThreadsQuery::default()
    };
    assert_eq!(alice.call(|c| c.threads(&q, None)).unwrap().len(), 1);
}

#[test]
fn a_name_being_approved_cannot_become_an_alias() {
    let clock = Clock::new();
    let (mut alice, _bob, _carol) = three(&clock);
    let mut dave = Daemon::new("dave", &clock, 4);
    ask(&mut dave, &mut alice);
    let id = alice.call(|c| c.requests()).unwrap().requests[0].id.clone();
    let pending = alice
        .call(|c| c.begin_approval(&id, Approval::default()))
        .unwrap();
    let e = set(&mut alice, "bob", &["dave"]).unwrap_err();
    assert!(
        e.kind == ErrorKind::Conflict && e.detail.contains("dave is being approved"),
        "{e:?}"
    );
    // So the approval still commits.
    let accepted = signed(&pending.call.headers, pending.call.path, &pending.call.body);
    dave.call(|c| {
        c.authenticate_peer(&accepted)?;
        c.accepted(&accepted.key, &offered(&pending.call.body))
    })
    .unwrap();
    assert_eq!(
        alice
            .call(|c| c.commit_approval(&pending))
            .unwrap()
            .name
            .as_str(),
        "dave"
    );
}

fn offer(d: &mut Daemon, names: &[&str]) -> Result<Aliases, cp_api::Error> {
    d.call(|c| c.set_own_aliases(&aliases(names)))
}

#[test]
fn peering_gives_each_side_the_aliases_the_other_offers() {
    let clock = Clock::new();
    let mut alice = Daemon::new("alice", &clock, 1);
    let mut bob = Daemon::new("bob", &clock, 2);
    offer(&mut alice, &["ally", "al"]).unwrap();
    offer(&mut bob, &["bobby"]).unwrap();
    assert_eq!(
        names(alice.call(|c| c.own_aliases()).unwrap().as_slice()),
        ["al", "ally"]
    );

    ask(&mut alice, &mut bob);
    assert_eq!(
        names(&bob.call(|c| c.requests()).unwrap().requests[0].aliases),
        ["al", "ally"]
    );
    let p = approve_asked(&mut alice, &mut bob, "alice");
    assert_eq!(
        names(&p.aliases),
        ["al", "ally"],
        "the asker's, on approval"
    );
    assert_eq!(
        names(&alice.call(|c| c.peer(&name("bob"), None)).unwrap().aliases),
        ["bobby"],
        "the approver's, with the acceptance"
    );
    // Later offers reach only new peers.
    offer(&mut alice, &["alf"]).unwrap();
    assert_eq!(
        names(&bob.call(|c| c.peer(&name("ally"), None)).unwrap().aliases),
        ["al", "ally"]
    );
}

#[test]
fn an_offered_alias_taken_here_is_skipped() {
    let clock = Clock::new();
    let (mut alice, _bob, _carol) = three(&clock);
    set(&mut alice, "bob", &["bobby"]).unwrap();
    let mut dave = Daemon::new("dave", &clock, 4);
    offer(
        &mut dave,
        &[
            "alice", "bob", "bobby", "carol", "d0", "d1", "d2", "d3", "d4", "d5",
        ],
    )
    .unwrap();

    let p = approve_as(&mut dave, &mut alice, "dave");
    // Taken here: the owner's name, peers' names, another peer's alias.
    assert_eq!(names(&p.aliases), ["d0", "d1", "d2", "d3", "d4", "d5"]);
}

#[test]
fn the_owner_offers_neither_their_name_nor_a_peers() {
    let clock = Clock::new();
    let (mut alice, _bob, _carol) = three(&clock);
    set(&mut alice, "bob", &["bobby"]).unwrap();
    for (alias, says) in [
        ("alice", "already the owner's name"),
        ("carol", "names the peer carol"),
        ("bobby", "names the peer bob"),
    ] {
        let e = offer(&mut alice, &["al", alias]).unwrap_err();
        assert!(
            e.kind == ErrorKind::Conflict && e.detail.contains(says),
            "{alias}: {e:?}"
        );
    }
    assert!(alice.call(|c| c.own_aliases()).unwrap().is_empty());
    offer(&mut alice, &["al"]).unwrap();
    offer(&mut alice, &[]).unwrap();
    assert!(alice.call(|c| c.own_aliases()).unwrap().is_empty());
}
