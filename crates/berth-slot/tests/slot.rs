//! Integration tests for the slot contract, run against the fake `slot.sh` fixture
//! and the reference scripts in `slots/examples/`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use berth_git::GitRunner;
use berth_slot::{
    Placement, RepoConfig, SlotEnv, SlotError, SlotScript, SlotType, SlotTypeSource,
    discover_slot_types, load_repo_config,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

// ---------------------------------------------------------------- helpers

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn examples_dir() -> PathBuf {
    manifest_dir().join("../../slots/examples")
}

/// git may not preserve the exec bit; set it again.
fn make_executable(path: &Path) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

fn load_type(dir: &Path) -> SlotType {
    make_executable(&dir.join("slot.sh"));
    SlotType::load(dir, SlotTypeSource::Global).unwrap()
}

fn fake_type() -> SlotType {
    load_type(&manifest_dir().join("tests/fixtures/fake"))
}

/// A throwaway slot type with the given `slot.sh` body.
fn custom_type(root: &Path, name: &str, script: &str, executable: bool) -> SlotType {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("slot.toml"), "agent_placement = \"host\"\n").unwrap();
    let sh = dir.join("slot.sh");
    fs::write(&sh, format!("#!/usr/bin/env bash\nset -eu\n{script}\n")).unwrap();
    let mode = if executable { 0o755 } else { 0o644 };
    fs::set_permissions(&sh, fs::Permissions::from_mode(mode)).unwrap();
    SlotType::load(&dir, SlotTypeSource::Repo).unwrap()
}

/// Keeps the user's git config out of the scripts' git commands.
fn hermetic_git_env() -> Vec<(String, String)> {
    vec![
        ("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned()),
        ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
    ]
}

fn slot_env(tmp: &Path, repo: &Path) -> SlotEnv {
    let mut extra = hermetic_git_env();
    extra.push(("FAKE_SECRET".to_owned(), "s3cret value".to_owned()));
    SlotEnv {
        repo: repo.to_path_buf(),
        slot_id: "slot-1".to_owned(),
        // Deliberately missing: `create` must make it.
        state_dir: tmp.join("state/slot-1"),
        base_ref: "main".to_owned(),
        branch: "berth/test".to_owned(),
        sidecar_bundle: tmp.join("bundle"),
        extra,
    }
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|&s| s.to_owned()).collect()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

fn write_type(root: &Path, name: &str, toml: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("slot.toml"), toml).unwrap();
}

/// Host-side git with the user's config excluded.
fn host_git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Berth Test",
            "-c",
            "user.email=test@berth.invalid",
        ])
        .args(args)
        .envs(hermetic_git_env())
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", text(&out.stderr));
    text(&out.stdout)
}

fn init_repo(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    host_git(dir, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(dir.join("README.md"), "hello\n").unwrap();
    host_git(dir, &["add", "README.md"]);
    host_git(dir, &["commit", "--quiet", "-m", "init"]);
}

// ---------------------------------------------------------------- discovery

#[test]
fn discovery_merges_global_and_repo_types() {
    let tmp = tempfile::tempdir().unwrap();
    let global = tmp.path().join("global");
    let repo = tmp.path().join("repo");
    write_type(
        &global,
        "alpha",
        "description = \"A\"\nagent_placement = \"host\"\n",
    );
    write_type(
        &global,
        "beta",
        "name = \"beta\"\ndescription = \"global beta\"\nagent_placement = \"inside\"\n",
    );
    fs::create_dir_all(global.join("not-a-type")).unwrap();
    fs::write(global.join("stray-file"), "").unwrap();
    let repo_slots = repo.join(".berth/slots");
    write_type(
        &repo_slots,
        "beta",
        "description = \"repo beta\"\nagent_placement = \"host\"\n",
    );
    write_type(&repo_slots, "gamma", "agent_placement = \"inside\"\n");

    let types = discover_slot_types(&global, &repo).unwrap();
    let names: Vec<&str> = types.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["alpha", "beta", "gamma"]);

    let (alpha, beta, gamma) = (&types[0], &types[1], &types[2]);
    assert_eq!(alpha.source, SlotTypeSource::Global);
    assert_eq!(alpha.description, "A");
    assert_eq!(alpha.agent_placement, Placement::Host);
    assert_eq!(alpha.script(), global.join("alpha/slot.sh"));
    assert_eq!(
        beta.source,
        SlotTypeSource::Repo,
        "repo type overrides global"
    );
    assert_eq!(beta.description, "repo beta");
    assert_eq!(beta.agent_placement, Placement::Host);
    assert_eq!(beta.dir, repo_slots.join("beta"));
    assert_eq!(gamma.description, "");
    assert_eq!(gamma.agent_placement, Placement::Inside);

    // Neither root existing is fine.
    let none = discover_slot_types(&tmp.path().join("nope"), &tmp.path().join("nope")).unwrap();
    assert!(none.is_empty());
}

#[test]
fn discovery_follows_symlinked_types() {
    let tmp = tempfile::tempdir().unwrap();
    let global = tmp.path().join("global");
    fs::create_dir_all(&global).unwrap();
    std::os::unix::fs::symlink(examples_dir().join("worktree"), global.join("worktree")).unwrap();
    let types = discover_slot_types(&global, &tmp.path().join("repo")).unwrap();
    assert_eq!(types.len(), 1);
    assert_eq!(types[0].name, "worktree");
}

#[test]
fn invalid_slot_toml_is_an_error_naming_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let global = tmp.path().join("global");
    write_type(&global, "good", "agent_placement = \"host\"\n");
    write_type(&global, "bad", "agent_placement = \"moon\"\n");
    let err = discover_slot_types(&global, tmp.path()).unwrap_err();
    let SlotError::Toml { path, .. } = &err else {
        panic!("expected Toml error, got {err:?}");
    };
    assert_eq!(path, &global.join("bad/slot.toml"));
    assert!(err.to_string().contains("bad/slot.toml"), "{err}");

    let missing = tmp.path().join("missing");
    write_type(&missing, "m", "description = \"no placement\"\n");
    let err = discover_slot_types(&missing, tmp.path()).unwrap_err();
    assert!(matches!(err, SlotError::Toml { .. }), "{err:?}");

    let mismatch = tmp.path().join("mismatch");
    write_type(
        &mismatch,
        "dir-name",
        "name = \"other\"\nagent_placement = \"host\"\n",
    );
    let err = discover_slot_types(&mismatch, tmp.path()).unwrap_err();
    let SlotError::InvalidSlotToml { path, message } = &err else {
        panic!("expected InvalidSlotToml, got {err:?}");
    };
    assert_eq!(path, &mismatch.join("dir-name/slot.toml"));
    assert!(message.contains("other"), "{message}");
}

#[test]
fn repo_config_loads_and_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(load_repo_config(tmp.path()).unwrap(), RepoConfig::default());

    let berth = tmp.path().join(".berth");
    fs::create_dir_all(&berth).unwrap();
    fs::write(berth.join("berth.toml"), "# no keys\n").unwrap();
    assert_eq!(load_repo_config(tmp.path()).unwrap().default_slot, None);

    fs::write(
        berth.join("berth.toml"),
        "default_slot = \"docker\"\n[future]\nkey = 1\n",
    )
    .unwrap();
    assert_eq!(
        load_repo_config(tmp.path())
            .unwrap()
            .default_slot
            .as_deref(),
        Some("docker")
    );

    fs::write(berth.join("berth.toml"), "default_slot = [").unwrap();
    let err = load_repo_config(tmp.path()).unwrap_err();
    let SlotError::Toml { path, .. } = err else {
        panic!("expected Toml error, got {err:?}");
    };
    assert_eq!(path, berth.join("berth.toml"));
}

// ---------------------------------------------------------------- fake slot.sh

#[tokio::test]
async fn fake_slot_lifecycle() {
    let tmp = tempfile::tempdir().unwrap();
    let env = slot_env(tmp.path(), &tmp.path().join("repo"));
    let checkout = env.state_dir.join("checkout");
    let script = SlotScript::new(fake_type(), env);

    let created = script.create().await.unwrap();
    assert_eq!(Path::new(&created.path), checkout);
    assert!(checkout.join(".git").is_dir());
    let script = script.with_checkout(created.path.clone());
    assert_eq!(script.checkout(), Some(created.path.as_str()));

    let status = script.status().await.unwrap().unwrap();
    assert!(status.ready);
    assert_eq!(status.detail, "checkout present");

    // Default cwd is the checkout.
    let out = script
        .exec(
            None,
            "git",
            &strings(&["symbolic-ref", "--short", "HEAD"]),
            None,
        )
        .await
        .unwrap();
    assert!(out.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "berth/test");

    // Exit codes and stderr propagate.
    let out = script
        .exec(None, "sh", &strings(&["-c", "echo oops >&2; exit 7"]), None)
        .await
        .unwrap();
    assert_eq!(out.status, 7);
    assert_eq!(text(&out.stderr), "oops");

    // stdin is piped through.
    let out = script
        .exec(None, "cat", &[], Some(b"line one\nline two\n".to_vec()))
        .await
        .unwrap();
    assert_eq!(out.stdout, b"line one\nline two\n");

    // --cwd overrides the checkout.
    let elsewhere = tmp.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let out = script
        .exec(elsewhere.to_str(), "pwd", &strings(&["-P"]), None)
        .await
        .unwrap();
    assert_eq!(
        PathBuf::from(text(&out.stdout)),
        elsewhere.canonicalize().unwrap()
    );

    // Contract variables and extra (secret) variables reach the command.
    let out = script
        .exec(
            None,
            "sh",
            &strings(&[
                "-c",
                r#"printf '%s|%s|%s|%s' "$BERTH_SLOT_ID" "$BERTH_BRANCH" "$FAKE_SECRET" "$BERTH_FORWARD_ENV""#,
            ]),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        text(&out.stdout),
        "slot-1|berth/test|s3cret value|GIT_CONFIG_GLOBAL GIT_CONFIG_NOSYSTEM FAKE_SECRET"
    );

    script.destroy().await.unwrap();
    assert!(!checkout.exists());
    script.destroy().await.unwrap(); // idempotent
    let status = script.status().await.unwrap().unwrap();
    assert!(!status.ready);
}

#[tokio::test]
async fn failing_create_surfaces_stderr() {
    let tmp = tempfile::tempdir().unwrap();
    let mut env = slot_env(tmp.path(), tmp.path());
    env.extra
        .push(("FAKE_FAIL_CREATE".to_owned(), "disk full".to_owned()));
    let err = SlotScript::new(fake_type(), env)
        .create()
        .await
        .unwrap_err();
    let SlotError::ScriptFailed {
        subcommand,
        status,
        stderr,
    } = &err
    else {
        panic!("expected ScriptFailed, got {err:?}");
    };
    assert_eq!(subcommand, "create");
    assert_eq!(*status, 3);
    assert_eq!(stderr, "fake create failed: disk full");
    assert!(err.to_string().contains("disk full"), "{err}");
}

#[tokio::test]
async fn create_rejects_output_without_path() {
    let tmp = tempfile::tempdir().unwrap();
    let ty = custom_type(tmp.path(), "nopath", r#"echo '{"dir": "/x"}'"#, true);
    let err = SlotScript::new(ty, slot_env(tmp.path(), tmp.path()))
        .create()
        .await
        .unwrap_err();
    assert!(
        matches!(&err, SlotError::BadOutput { subcommand, .. } if subcommand == "create"),
        "{err:?}"
    );
}

#[tokio::test]
async fn status_is_optional() {
    let tmp = tempfile::tempdir().unwrap();
    let env = slot_env(tmp.path(), tmp.path());

    let unsupported = custom_type(
        tmp.path(),
        "unsupported",
        r#"echo "unknown subcommand $1" >&2; exit 64"#,
        true,
    );
    let status = SlotScript::new(unsupported, env.clone()).status().await;
    assert_eq!(status.unwrap(), None);

    let garbled = custom_type(tmp.path(), "garbled", "echo 'all good'", true);
    let status = SlotScript::new(garbled, env).status().await;
    assert_eq!(status.unwrap(), None);
}

#[tokio::test]
async fn non_executable_script_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let ty = custom_type(tmp.path(), "noexec", "exit 0", false);
    let script_path = ty.script();
    let script = SlotScript::new(ty, slot_env(tmp.path(), tmp.path()));
    let err = script.create().await.unwrap_err();
    assert!(
        matches!(&err, SlotError::NotExecutable(p) if *p == script_path),
        "{err:?}"
    );
    let err = script.exec(None, "true", &[], None).await.unwrap_err();
    assert!(matches!(err, SlotError::NotExecutable(_)), "{err:?}");
}

#[tokio::test]
async fn lifecycle_timeout_kills_the_script_and_its_children() {
    let tmp = tempfile::tempdir().unwrap();
    let ty = custom_type(
        tmp.path(),
        "slow",
        r#"sleep 30 & echo $! > "$BERTH_SLOT_STATE/child.pid"; wait"#,
        true,
    );
    let env = slot_env(tmp.path(), tmp.path());
    let pid_file = env.state_dir.join("child.pid");
    let script = SlotScript::new(ty, env).with_timeout(Duration::from_millis(500));
    let started = std::time::Instant::now();
    let err = script.create().await.unwrap_err();
    assert!(
        matches!(&err, SlotError::Timeout { subcommand, .. } if subcommand == "create"),
        "{err:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));

    let pid = fs::read_to_string(&pid_file).unwrap().trim().to_owned();
    let mut alive = true;
    for _ in 0..50 {
        let probe = std::process::Command::new("/bin/kill")
            .args(["-0", &pid])
            .stderr(Stdio::null())
            .status()
            .unwrap();
        if !probe.success() {
            alive = false;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!alive, "grandchild {pid} survived the timeout");
}

#[tokio::test]
async fn git_runner_runs_git_in_the_checkout_with_env() {
    let tmp = tempfile::tempdir().unwrap();
    let script = SlotScript::new(fake_type(), slot_env(tmp.path(), tmp.path()));
    let created = script.create().await.unwrap();

    let out = script
        .run_git(&strings(&["rev-parse", "--show-toplevel"]), &[])
        .await
        .unwrap();
    assert!(out.success(), "{}", text(&out.stderr));
    assert_eq!(
        PathBuf::from(text(&out.stdout)),
        Path::new(&created.path).canonicalize().unwrap()
    );

    // `env` must reach git itself, not just slot.sh.
    let git_env = vec![
        ("GIT_AUTHOR_NAME".to_owned(), "Slot Author".to_owned()),
        (
            "GIT_AUTHOR_EMAIL".to_owned(),
            "slot@berth.invalid".to_owned(),
        ),
    ];
    let out = script
        .run_git(&strings(&["var", "GIT_AUTHOR_IDENT"]), &git_env)
        .await
        .unwrap();
    assert!(out.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).starts_with("Slot Author <slot@berth.invalid>"),
        "{}",
        text(&out.stdout)
    );

    // git's own failures come back as output, not as runner errors.
    let out = script
        .run_git(&strings(&["rev-parse", "--verify", "--quiet", "nope"]), &[])
        .await
        .unwrap();
    assert!(!out.success());

    script.destroy().await.unwrap();
}

#[tokio::test]
async fn exec_command_hosts_a_long_lived_line_protocol() {
    let tmp = tempfile::tempdir().unwrap();
    let script = SlotScript::new(fake_type(), slot_env(tmp.path(), tmp.path()));
    script.create().await.unwrap();

    let echo_loop = r#"while IFS= read -r line; do printf 'echo:%s\n' "$line"; done; echo bye"#;
    let mut child = script
        .exec_command(None, "sh", &strings(&["-c", echo_loop]))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();

    let round_trip = async {
        for msg in [
            r#"{"id":"1","type":"session.start"}"#,
            r#"{"id":"2","type":"session.turn"}"#,
        ] {
            stdin
                .write_all(format!("{msg}\n").as_bytes())
                .await
                .unwrap();
            stdin.flush().await.unwrap();
            let line = lines.next_line().await.unwrap().unwrap();
            assert_eq!(line, format!("echo:{msg}"));
        }
        drop(stdin); // EOF ends the loop
        assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("bye"));
        assert_eq!(lines.next_line().await.unwrap(), None);
        child.wait().await.unwrap()
    };
    let status = tokio::time::timeout(Duration::from_secs(20), round_trip)
        .await
        .expect("line protocol round trip timed out");
    assert!(status.success());

    script.destroy().await.unwrap();
}

// ---------------------------------------------------------------- reference scripts

#[test]
fn example_types_load_and_parse() {
    let expected = [
        ("worktree", Placement::Host),
        ("docker", Placement::Inside),
        ("vm", Placement::Host),
    ];
    let types = discover_slot_types(&examples_dir(), Path::new("/nonexistent")).unwrap();
    assert_eq!(types.len(), expected.len());
    for (name, placement) in expected {
        let ty = types.iter().find(|t| t.name == name).unwrap();
        assert_eq!(ty.agent_placement, placement, "{name}");
        make_executable(&ty.script());
        ty.ensure_executable().unwrap();
        let check = std::process::Command::new("bash")
            .arg("-n")
            .arg(ty.script())
            .output()
            .unwrap();
        assert!(check.status.success(), "{name}: {}", text(&check.stderr));
    }
}

#[tokio::test]
async fn worktree_example_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);
    let ty = load_type(&examples_dir().join("worktree"));
    let env = slot_env(tmp.path(), &repo);
    let checkout = env.state_dir.join("checkout");
    let script = SlotScript::new(ty, env);

    let created = script.create().await.unwrap();
    assert_eq!(Path::new(&created.path), checkout);
    assert!(checkout.join("README.md").is_file());

    let out = script
        .exec(
            None,
            "git",
            &strings(&["status", "--porcelain=v1", "--branch"]),
            None,
        )
        .await
        .unwrap();
    assert!(out.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "## berth/test");

    // The base is not the new branch's upstream.
    let out = script
        .run_git(
            &strings(&["rev-parse", "--abbrev-ref", "berth/test@{upstream}"]),
            &[],
        )
        .await
        .unwrap();
    assert!(!out.success());

    let status = script.status().await.unwrap().unwrap();
    assert!(status.ready, "{}", status.detail);

    // Snapshot-style git with a private index (AGENTS.md §7) works through the slot.
    fs::write(checkout.join("new.txt"), "new\n").unwrap();
    let index = vec![(
        "GIT_INDEX_FILE".to_owned(),
        script.env.state_dir.join("snap.idx").display().to_string(),
    )];
    let add = script
        .run_git(&strings(&["add", "-A"]), &index)
        .await
        .unwrap();
    assert!(add.success(), "{}", text(&add.stderr));
    let tree = script
        .run_git(&strings(&["write-tree"]), &index)
        .await
        .unwrap();
    assert!(tree.success(), "{}", text(&tree.stderr));
    assert_eq!(text(&tree.stdout).len(), 40);
    let porcelain = script
        .run_git(&strings(&["status", "--porcelain"]), &[])
        .await
        .unwrap();
    assert_eq!(
        text(&porcelain.stdout),
        "?? new.txt",
        "real index untouched"
    );

    script.destroy().await.unwrap();
    assert!(!checkout.exists());
    script.destroy().await.unwrap(); // idempotent
    let worktrees = host_git(&repo, &["worktree", "list", "--porcelain"]);
    assert_eq!(worktrees.matches("worktree ").count(), 1, "{worktrees}");
    assert!(!script.status().await.unwrap().unwrap().ready);

    // The branch survives destroy; creating again checks it out.
    host_git(&repo, &["rev-parse", "--verify", "refs/heads/berth/test"]);
    script.create().await.unwrap();
    let out = script
        .exec(
            None,
            "git",
            &strings(&["symbolic-ref", "--short", "HEAD"]),
            None,
        )
        .await
        .unwrap();
    assert_eq!(text(&out.stdout), "berth/test");
    script.destroy().await.unwrap();
}
