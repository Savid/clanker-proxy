//! Agent tokens.
#![allow(clippy::unwrap_used)]

mod common;

use common::{Clock, Daemon, kind, name, peer, scope};
use cp_api::{AgentTokenInput, ErrorKind, OwnWebhook, WebhookSecret, WebhookUrl};
use cp_core::Millis;

#[test]
fn tokens_authenticate_until_revoked_or_expired() {
    let clock = Clock::new();
    let (mut alice, mut bob) = (
        Daemon::new("alice", &clock, 1),
        Daemon::new("bob", &clock, 2),
    );
    peer(&mut alice, &mut bob);

    let input = |n: &str, peers: &[&str], expires: Option<i64>| AgentTokenInput {
        name: name(n),
        peers: scope(peers),
        expires_at: expires.map(|ms| clock.now().plus(ms).at()),
    };
    let t = alice
        .call(|c| c.create_agent_token(input("runner", &["bob"], None)))
        .unwrap();
    let agent = alice
        .call(|c| c.authenticate_agent(t.token.expose()))
        .unwrap();
    assert_eq!(agent.peers, scope(&["bob"]));
    assert!(
        !format!("{t:?}").contains(t.token.expose()),
        "the token stays out of Debug"
    );

    assert_eq!(
        kind(&alice.call(|c| c.create_agent_token(input("runner", &[], None)))),
        ErrorKind::Conflict
    );
    assert_eq!(
        kind(&alice.call(|c| c.create_agent_token(input("x", &["carol"], None)))),
        ErrorKind::Invalid,
        "unknown peer"
    );
    assert_eq!(
        kind(&alice.call(|c| c.create_agent_token(input("x", &[], Some(-1000))))),
        ErrorKind::Invalid,
        "past expiry"
    );

    let short = alice
        .call(|c| c.create_agent_token(input("short", &[], Some(Millis::HOUR))))
        .unwrap();
    clock.advance(2 * Millis::HOUR);
    assert_eq!(
        kind(&alice.call(|c| c.authenticate_agent(short.token.expose()))),
        ErrorKind::Unauthorized
    );
    alice
        .call(|c| c.create_agent_token(input("short", &[], None)))
        .unwrap();

    let listed = alice.call(|c| c.agent_tokens()).unwrap().tokens;
    assert_eq!(
        listed.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        ["runner", "short"]
    );
    assert!(listed[0].used_at.is_some());

    alice
        .call(|c| c.delete_agent_token(&name("runner")))
        .unwrap();
    assert_eq!(
        kind(&alice.call(|c| c.authenticate_agent(t.token.expose()))),
        ErrorKind::Unauthorized
    );
    assert_eq!(
        kind(&alice.call(|c| c.delete_agent_token(&name("runner")))),
        ErrorKind::NotFound
    );
    assert_eq!(
        kind(&alice.call(|c| c.authenticate_agent("cpo_owner"))),
        ErrorKind::Unauthorized
    );
}

#[test]
fn a_token_revokes_itself_and_its_webhook() {
    let clock = Clock::new();
    let (mut alice, mut bob) = (
        Daemon::new("alice", &clock, 1),
        Daemon::new("bob", &clock, 2),
    );
    peer(&mut alice, &mut bob);
    let make = |alice: &mut Daemon| {
        alice
            .call(|c| {
                c.create_agent_token(AgentTokenInput {
                    name: name("runner"),
                    peers: scope(&[]),
                    expires_at: None,
                })
            })
            .unwrap()
    };
    let t = make(&mut alice);
    alice
        .call(|c| {
            c.set_agent_webhook(
                &name("runner"),
                &scope(&[]),
                &OwnWebhook {
                    url: WebhookUrl::new("https://runner.example.com/hook".into()).unwrap(),
                    secret: WebhookSecret::new(
                        "YWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWE=".into(),
                    )
                    .unwrap(),
                },
            )
        })
        .unwrap();
    assert_eq!(alice.call(|c| c.webhooks()).unwrap().len(), 1);

    alice
        .call(|c| c.revoke_own_agent_token(t.token.expose()))
        .unwrap();
    assert_eq!(
        kind(&alice.call(|c| c.authenticate_agent(t.token.expose()))),
        ErrorKind::Unauthorized
    );
    assert_eq!(alice.call(|c| c.webhooks()).unwrap().len(), 0);
    assert_eq!(alice.call(|c| c.agent_tokens()).unwrap().tokens.len(), 0);
    assert_eq!(
        kind(&alice.call(|c| c.revoke_own_agent_token(t.token.expose()))),
        ErrorKind::Unauthorized
    );

    // A token revoked by the owner, whose name went to a new token, cannot
    // revoke the new one.
    let old = make(&mut alice);
    alice
        .call(|c| c.delete_agent_token(&name("runner")))
        .unwrap();
    let new = make(&mut alice);
    assert_eq!(
        kind(&alice.call(|c| c.revoke_own_agent_token(old.token.expose()))),
        ErrorKind::Unauthorized
    );
    alice
        .call(|c| c.authenticate_agent(new.token.expose()))
        .unwrap();

    // An expired token may still revoke itself, and only that.
    alice
        .call(|c| c.delete_agent_token(&name("runner")))
        .unwrap();
    let lapsing = alice
        .call(|c| {
            c.create_agent_token(AgentTokenInput {
                name: name("lapsing"),
                peers: scope(&[]),
                expires_at: Some(clock.now().plus(Millis::HOUR).at()),
            })
        })
        .unwrap();
    clock.advance(2 * Millis::HOUR);
    let token = lapsing.token.expose();
    assert_eq!(
        kind(&alice.call(|c| c.authenticate_agent(token))),
        ErrorKind::Unauthorized
    );
    alice
        .call(|c| c.authenticate_agent_to_revoke(token))
        .unwrap();
    alice.call(|c| c.revoke_own_agent_token(token)).unwrap();
    assert_eq!(
        kind(&alice.call(|c| c.authenticate_agent_to_revoke(token))),
        ErrorKind::Unauthorized,
        "a revoked token is unknown"
    );
}
