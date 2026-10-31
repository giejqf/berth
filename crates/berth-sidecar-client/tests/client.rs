#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use berth_protocol::{AppToSidecar, SessionStart, SessionTurn, SidecarToApp};
use berth_sidecar_client::{Sidecar, SidecarEvent};
use tokio::process::Command;

fn fake() -> Command {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/fake_sidecar.sh"
    );
    let mut cmd = Command::new("/bin/sh");
    cmd.arg(path);
    cmd
}

fn start() -> AppToSidecar {
    AppToSidecar::SessionStart(SessionStart {
        workflow_id: "w".into(),
        cwd: "/tmp".into(),
        system_prompt: String::new(),
        env: Default::default(),
        allowed_tools: vec![],
        connector_skills: vec![],
        model: None,
        effort: None,
        title: None,
    })
}

#[tokio::test]
async fn request_reply_events_and_exit() {
    let (sidecar, mut events) = Sidecar::spawn(fake()).unwrap();
    assert!(sidecar.pid().is_some());

    let reply = sidecar
        .request(start(), Duration::from_secs(5))
        .await
        .unwrap();
    assert!(
        matches!(reply.body, SidecarToApp::SessionStarted(ref s) if s.claude_session_id == "sess")
    );
    // The reply is also on the event stream.
    let Some(SidecarEvent::Message(m)) = events.recv().await else {
        panic!()
    };
    assert_eq!(m.reply_to, reply.reply_to);

    sidecar
        .send(AppToSidecar::SessionTurn(SessionTurn {
            text: "hi".into(),
            attachments: vec![],
            uuid: None,
        }))
        .unwrap();
    let Some(SidecarEvent::ProtocolError { line, .. }) = events.recv().await else {
        panic!()
    };
    assert_eq!(line, "not json");
    let Some(SidecarEvent::Message(m)) = events.recv().await else {
        panic!()
    };
    assert!(matches!(m.body, SidecarToApp::RawEvent(ref r) if r.json["echo"] == true));

    sidecar.shutdown(Duration::from_secs(5)).await;
    let mut exit = None;
    while let Some(e) = events.recv().await {
        if let SidecarEvent::Exited { code, stderr_tail } = e {
            exit = Some((code, stderr_tail));
        }
    }
    let (code, tail) = exit.unwrap();
    assert_eq!(code, Some(3));
    assert!(
        tail.contains("fake sidecar starting") && tail.contains("stopping"),
        "{tail}"
    );
}

#[tokio::test]
async fn request_times_out_and_kill_ends_the_process() {
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "cat > /dev/null"]);
    let (sidecar, mut events) = Sidecar::spawn(cmd).unwrap();
    let err = sidecar
        .request(start(), Duration::from_millis(100))
        .await
        .unwrap_err();
    assert!(matches!(err, berth_sidecar_client::ClientError::Timeout(_)));
    sidecar.kill().await;
    let last = tokio::time::timeout(Duration::from_secs(5), async {
        let mut last = None;
        while let Some(e) = events.recv().await {
            last = Some(e);
        }
        last
    })
    .await
    .unwrap();
    assert!(matches!(last, Some(SidecarEvent::Exited { .. })));
}

#[tokio::test]
async fn spawn_failure_is_reported() {
    let cmd = Command::new("/nonexistent/sidecar");
    assert!(Sidecar::spawn(cmd).is_err());
}

#[tokio::test]
async fn a_stray_byte_does_not_stop_the_reading() {
    // Not UTF-8 on both pipes, then more stderr than its pipe holds: a
    // reader that stopped at the bad byte leaves the sidecar blocked.
    let mut cmd = Command::new("/bin/sh");
    cmd.args([
        "-c",
        r#"printf 'oops \377\n' >&2
           printf 'caf\351 \n'
           head -c 200000 /dev/zero | tr '\0' x >&2
           printf '\nlast words\n' >&2
           printf '{"id":"r1","type":"raw.event","seq":1,"kind":"assistant","json":{}}\n'
           exit 4"#,
    ]);
    let (_sidecar, mut events) = Sidecar::spawn(cmd).unwrap();
    let all = tokio::time::timeout(Duration::from_secs(10), async {
        let mut all = vec![];
        while let Some(e) = events.recv().await {
            all.push(e);
        }
        all
    })
    .await
    .expect("the sidecar ran to its end");
    assert!(matches!(&all[0], SidecarEvent::ProtocolError { line, .. } if line == "caf\u{fffd} "));
    assert!(matches!(&all[1], SidecarEvent::Message(_)));
    let Some(SidecarEvent::Exited { code, stderr_tail }) = all.last() else {
        panic!("{all:?}")
    };
    assert_eq!(*code, Some(4));
    assert!(stderr_tail.ends_with("last words"), "{stderr_tail}");
}
