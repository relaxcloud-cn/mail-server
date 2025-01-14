/*
 * SPDX-FileCopyrightText: 2020 Stalwart Labs Ltd <hello@stalw.art>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-SEL
 */

use std::time::{Duration, Instant};

use common::{config::server::ServerProtocol, ipc::QueueEvent};
use mail_auth::MX;

use crate::smtp::{session::TestSession, TestSMTP};
use smtp::queue::manager::Queue;

const LOCAL: &str = r#"
[spam-filter]
enable = false

[session.rcpt]
relay = true

[session.data.limits]
messages = 2000

[queue.outbound]
concurrency = 1

"#;

const REMOTE: &str = r#"
[session.ehlo]
reject-non-fqdn = false

[session.rcpt]
relay = true

[spam-filter]
enable = false

"#;

#[tokio::test]
#[serial_test::serial]
async fn concurrent_queue() {
    // Enable logging
    crate::enable_logging();

    // Start test server
    let remote = TestSMTP::new("smtp_concurrent_queue_remote", REMOTE).await;
    let _rx = remote.start(&[ServerProtocol::Smtp]).await;

    let local = TestSMTP::new("smtp_concurrent_queue_local", LOCAL).await;

    // Add mock DNS entries
    let core = local.build_smtp();
    core.core.smtp.resolvers.dns.mx_add(
        "foobar.org",
        vec![MX {
            exchanges: vec!["mx.foobar.org".to_string()],
            preference: 10,
        }],
        Instant::now() + Duration::from_secs(100),
    );
    core.core.smtp.resolvers.dns.ipv4_add(
        "mx.foobar.org",
        vec!["127.0.0.1".parse().unwrap()],
        Instant::now() + Duration::from_secs(100),
    );

    let mut session = local.new_session();
    session.data.remote_ip_str = "10.0.0.1".to_string();
    session.eval_session_params().await;
    session.ehlo("mx.test.org").await;

    // Spawn 20 concurrent queues
    let mut inners = vec![];
    for _ in 0..20 {
        let (inner, rxs) = local.inner_with_rxs();
        inners.push(inner.clone());
        tokio::spawn(async move {
            Queue::new(inner, rxs.queue_rx.unwrap()).start().await;
        });
    }

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Send 1000 test messages
    for _ in 0..100 {
        session
            .send_message("john@test.org", &["bill@foobar.org"], "test:no_dkim", "250")
            .await;
    }

    // Wake up all queues
    for inner in &inners {
        inner
            .ipc
            .queue_tx
            .send(QueueEvent::Refresh(None))
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(1500)).await;

    local.queue_receiver.assert_queue_is_empty().await;
    let remote_messages = remote.queue_receiver.read_queued_messages().await;
    assert_eq!(remote_messages.len(), 100);

    // Make sure local store is queue
    core.core
        .storage
        .data
        .assert_is_empty(core.core.storage.blob.clone())
        .await;
}
