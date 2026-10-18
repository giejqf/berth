//! Connector discovery, skills, origins and secrets against temp-dir fixtures.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use berth_connectors::{
    Connector, ConnectorError, KeychainStore, MemoryStore, OriginItem, OriginSource, SecretStore,
    default_connectors_dir, discover, path_with_connectors, secret_env,
};
use tempfile::TempDir;

/// Writes an executable script.
fn write_exe(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Creates `<root>/<dir>/` with `connector.toml` and (if `bin` is set) `bin/<bin>`.
fn connector_dir(root: &Path, dir: &str, toml: &str, bin: Option<(&str, &str)>) -> PathBuf {
    let path = root.join(dir);
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("connector.toml"), toml).unwrap();
    if let Some((name, script)) = bin {
        write_exe(&path.join("bin").join(name), script);
    }
    path
}

const LINEAR_BIN: &str = r#"#!/bin/sh
if [ "$1 $2 $3" != "issues list --json" ]; then
  echo "bad args: $*" >&2
  exit 2
fi
printf '[{"ref":"LIN-1","title":"Fix login","body":"key=%s","url":"https://linear.app/x/LIN-1","extra":1},{"ref":"LIN-2","title":"Docs"}]' "${LINEAR_API_KEY:-unset}"
"#;

const LINEAR_TOML: &str = r#"
name = "linear"
description = "Linear issues"
binary = "bin/linear-cli"
secrets = ["LINEAR_API_KEY"]

[origin]
kind = "linear"
list = ["issues", "list", "--json"]
"#;

fn linear(root: &Path) -> PathBuf {
    let dir = connector_dir(
        root,
        "linear",
        LINEAR_TOML,
        Some(("linear-cli", LINEAR_BIN)),
    );
    fs::write(dir.join("SKILL.md"), "# Linear\n\nUse `linear-cli`.\n").unwrap();
    dir
}

fn simple(root: &Path, name: &str, secrets: &[&str]) -> PathBuf {
    let secrets = secrets
        .iter()
        .map(|s| format!("{s:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let toml = format!("name = \"{name}\"\nbinary = \"bin/{name}\"\nsecrets = [{secrets}]\n");
    connector_dir(root, name, &toml, Some((name, "#!/bin/sh\nexit 0\n")))
}

fn only_error(tmp: &TempDir) -> (PathBuf, ConnectorError) {
    let mut d = discover(tmp.path());
    assert!(d.connectors.is_empty(), "{:?}", d.connectors);
    assert_eq!(d.errors.len(), 1, "{:?}", d.errors);
    d.errors.remove(0)
}

fn invalid_message(err: ConnectorError) -> String {
    match err {
        ConnectorError::Invalid { message, .. } => message,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn default_dir() {
    assert!(default_connectors_dir().ends_with(".config/berth/connectors"));
}

#[test]
fn missing_dir_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let d = discover(&tmp.path().join("nope"));
    assert!(d.connectors.is_empty());
    assert!(d.errors.is_empty());
}

#[test]
fn discovers_valid_connector() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = linear(tmp.path());
    let d = discover(tmp.path());
    assert!(d.errors.is_empty(), "{:?}", d.errors);
    assert_eq!(
        d.connectors,
        vec![Connector {
            name: "linear".into(),
            description: "Linear issues".into(),
            dir: dir.clone(),
            binary: dir.join("bin/linear-cli"),
            secrets: vec!["LINEAR_API_KEY".into()],
            skill_md: Some(dir.join("SKILL.md")),
            origin: Some(OriginSource {
                kind: "linear".into(),
                list: vec!["issues".into(), "list".into(), "--json".into()],
            }),
        }]
    );
    let c = &d.connectors[0];
    assert!(c.binary.is_absolute());
    assert_eq!(c.bin_dir(), dir.join("bin"));
    assert_eq!(c.keychain_service(), "com.berth.connector.linear");
}

#[test]
fn minimal_connector_and_absolute_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let exe = tmp.path().join("elsewhere/tool");
    write_exe(&exe, "#!/bin/sh\n");
    let toml = format!("name = \"tool\"\nbinary = {:?}\n", exe.to_str().unwrap());
    connector_dir(&tmp.path().join("connectors"), "tool", &toml, None);
    let d = discover(&tmp.path().join("connectors"));
    assert!(d.errors.is_empty(), "{:?}", d.errors);
    let c = &d.connectors[0];
    assert_eq!(c.binary, exe);
    assert_eq!(c.bin_dir(), tmp.path().join("elsewhere"));
    assert_eq!(c.description, "");
    assert!(c.secrets.is_empty());
    assert_eq!(c.skill_md, None);
    assert_eq!(c.origin, None);
    assert_eq!(c.skill_install().unwrap(), None);
}

#[test]
fn bad_connectors_do_not_hide_good_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    simple(root, "zeta", &[]);
    linear(root);
    let missing_bin = connector_dir(
        root,
        "missing-bin",
        "name = \"mb\"\nbinary = \"bin/x\"\n",
        None,
    );
    let bad_toml = connector_dir(root, "bad-toml", "name = \"bt\"\nbinary = ", None);
    let bad_name = connector_dir(
        root,
        "bad-name",
        "name = \"../evil\"\nbinary = \"bin/x\"\n",
        Some(("x", "#!/bin/sh\n")),
    );
    let no_toml = root.join("no-toml");
    fs::create_dir(&no_toml).unwrap();
    fs::create_dir(root.join(".hidden")).unwrap();
    fs::write(root.join("README"), "not a connector").unwrap();

    let d = discover(root);
    let names: Vec<&str> = d.connectors.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["linear", "zeta"]);

    let paths: Vec<&Path> = d.errors.iter().map(|(p, _)| p.as_path()).collect();
    assert_eq!(
        paths,
        [bad_name.as_path(), &bad_toml, &missing_bin, &no_toml]
    );
    assert!(matches!(d.errors[0].1, ConnectorError::Invalid { .. }));
    assert!(matches!(d.errors[1].1, ConnectorError::Toml { .. }));
    assert!(matches!(d.errors[2].1, ConnectorError::Invalid { .. }));
    assert!(matches!(d.errors[3].1, ConnectorError::Invalid { .. }));
}

#[test]
fn missing_binary() {
    let tmp = tempfile::tempdir().unwrap();
    connector_dir(tmp.path(), "a", "name = \"a\"\nbinary = \"bin/a\"\n", None);
    let msg = invalid_message(only_error(&tmp).1);
    assert!(msg.contains("does not exist"), "{msg}");
}

#[test]
fn non_executable_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = connector_dir(tmp.path(), "a", "name = \"a\"\nbinary = \"a.sh\"\n", None);
    fs::write(dir.join("a.sh"), "#!/bin/sh\n").unwrap();
    fs::set_permissions(dir.join("a.sh"), fs::Permissions::from_mode(0o644)).unwrap();
    let msg = invalid_message(only_error(&tmp).1);
    assert!(msg.contains("not executable"), "{msg}");
}

#[test]
fn bad_toml() {
    let tmp = tempfile::tempdir().unwrap();
    connector_dir(tmp.path(), "a", "name = [", None);
    let (_, err) = only_error(&tmp);
    assert!(matches!(err, ConnectorError::Toml { .. }), "{err:?}");
}

#[test]
fn unknown_field_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    connector_dir(
        tmp.path(),
        "a",
        "name = \"a\"\nbinary = \"bin/a\"\nsecret = [\"X\"]\n",
        Some(("a", "#!/bin/sh\n")),
    );
    let (_, err) = only_error(&tmp);
    assert!(matches!(err, ConnectorError::Toml { .. }), "{err:?}");
}

#[test]
fn bad_names() {
    for name in ["", "a b", "a/b", "..", "x'y"] {
        let tmp = tempfile::tempdir().unwrap();
        let toml = format!("name = {name:?}\nbinary = \"bin/a\"\n");
        connector_dir(tmp.path(), "a", &toml, Some(("a", "#!/bin/sh\n")));
        let msg = invalid_message(only_error(&tmp).1);
        assert!(msg.contains("must match"), "{name:?}: {msg}");
    }
}

#[test]
fn bad_secret_names() {
    for secret in ["lower", "1ABC", "A-B", ""] {
        let tmp = tempfile::tempdir().unwrap();
        simple(tmp.path(), "a", &[secret]);
        let msg = invalid_message(only_error(&tmp).1);
        assert!(msg.contains("env var name"), "{secret:?}: {msg}");
    }
}

#[test]
fn duplicate_names() {
    let tmp = tempfile::tempdir().unwrap();
    simple(tmp.path(), "a", &[]);
    connector_dir(
        tmp.path(),
        "b",
        "name = \"a\"\nbinary = \"bin/b\"\n",
        Some(("b", "#!/bin/sh\n")),
    );
    let d = discover(tmp.path());
    assert_eq!(d.connectors.len(), 1);
    assert_eq!(d.connectors[0].dir, tmp.path().join("a"));
    assert_eq!(d.errors.len(), 1);
    assert_eq!(d.errors[0].0, tmp.path().join("b"));
}

#[test]
fn empty_origin_kind() {
    let tmp = tempfile::tempdir().unwrap();
    connector_dir(
        tmp.path(),
        "a",
        "name = \"a\"\nbinary = \"bin/a\"\n[origin]\nkind = \"\"\n",
        Some(("a", "#!/bin/sh\n")),
    );
    let msg = invalid_message(only_error(&tmp).1);
    assert!(msg.contains("origin.kind"), "{msg}");
}

#[test]
fn skill_install_script_writes_skill() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = linear(&tmp.path().join("connectors"));
    let c = Connector::load(&dir).unwrap();
    let install = c.skill_install().unwrap().unwrap();
    assert_eq!(install.stdin, fs::read(dir.join("SKILL.md")).unwrap());

    let checkout = tmp.path().join("checkout");
    fs::create_dir(&checkout).unwrap();
    // Run twice: install must be idempotent (overwrite).
    for _ in 0..2 {
        let [sh, flag, script] = install.argv();
        let mut child = Command::new(sh)
            .args([flag, script])
            .current_dir(&checkout)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&install.stdin)
            .unwrap();
        assert!(child.wait().unwrap().success());
    }
    let installed = checkout.join(".claude/skills/linear/SKILL.md");
    assert_eq!(fs::read(&installed).unwrap(), install.stdin);
    let leftovers: Vec<_> = fs::read_dir(installed.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(leftovers, ["SKILL.md"]);
}

#[test]
fn skill_install_rejects_injected_name() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = linear(tmp.path());
    let mut c = Connector::load(&dir).unwrap();
    c.name = "x'; rm -rf / #".into();
    let err = c.skill_install().unwrap_err();
    assert!(matches!(err, ConnectorError::Invalid { .. }), "{err:?}");
}

#[tokio::test]
async fn list_origins_parses_items_and_passes_env() {
    let tmp = tempfile::tempdir().unwrap();
    let c = Connector::load(&linear(tmp.path())).unwrap();
    let env = vec![("LINEAR_API_KEY".to_owned(), "k123".to_owned())];
    let items = c.list_origins(&env).await.unwrap();
    assert_eq!(
        items,
        vec![
            OriginItem {
                r#ref: "LIN-1".into(),
                title: "Fix login".into(),
                body: Some("key=k123".into()),
                url: Some("https://linear.app/x/LIN-1".into()),
            },
            OriginItem {
                r#ref: "LIN-2".into(),
                title: "Docs".into(),
                body: None,
                url: None,
            },
        ]
    );
    let json = serde_json::to_value(&items[1]).unwrap();
    assert_eq!(json["ref"], "LIN-2");
}

#[tokio::test]
async fn list_origins_failure_carries_stderr() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = Connector::load(&linear(tmp.path())).unwrap();
    c.origin.as_mut().unwrap().list = vec!["nope".into()];
    match c.list_origins(&[]).await.unwrap_err() {
        ConnectorError::Failed { status, stderr } => {
            assert_eq!(status, 2);
            assert_eq!(stderr, "bad args: nope");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn list_origins_bad_json() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = connector_dir(
        tmp.path(),
        "a",
        "name = \"a\"\nbinary = \"bin/a\"\n[origin]\nkind = \"a\"\n",
        Some(("a", "#!/bin/sh\necho '{\"not\":\"an array\"}'\n")),
    );
    let c = Connector::load(&dir).unwrap();
    let err = c.list_origins(&[]).await.unwrap_err();
    assert!(matches!(err, ConnectorError::Json(_)), "{err:?}");
}

#[tokio::test]
async fn list_origins_without_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let c = Connector::load(&simple(tmp.path(), "a", &[])).unwrap();
    let err = c.list_origins(&[]).await.unwrap_err();
    assert!(matches!(err, ConnectorError::Invalid { .. }), "{err:?}");
}

#[test]
fn secret_env_collects_and_reports_missing() {
    let tmp = tempfile::tempdir().unwrap();
    linear(tmp.path());
    simple(tmp.path(), "posthog", &["POSTHOG_KEY", "POSTHOG_HOST"]);
    simple(tmp.path(), "zz-dup", &["LINEAR_API_KEY"]);
    let connectors = discover(tmp.path()).connectors;

    let store = MemoryStore::new();
    store
        .set("com.berth.connector.linear", "LINEAR_API_KEY", "lin-secret")
        .unwrap();
    store
        .set("com.berth.connector.posthog", "POSTHOG_KEY", "ph-secret")
        .unwrap();
    store
        .set("com.berth.connector.zz-dup", "LINEAR_API_KEY", "other")
        .unwrap();

    let env = secret_env(&connectors, &store).unwrap();
    assert_eq!(
        env.vars,
        vec![
            ("LINEAR_API_KEY".to_owned(), "lin-secret".to_owned()),
            ("POSTHOG_KEY".to_owned(), "ph-secret".to_owned()),
        ]
    );
    assert_eq!(
        env.missing,
        vec![("posthog".to_owned(), "POSTHOG_HOST".to_owned())]
    );
    let debug = format!("{env:?}");
    assert!(debug.contains("LINEAR_API_KEY"));
    assert!(!debug.contains("lin-secret") && !debug.contains("ph-secret"));
}

#[test]
fn memory_store_round_trip() {
    let store = MemoryStore::new();
    assert_eq!(store.get("s", "a").unwrap(), None);
    store.set("s", "a", "v1").unwrap();
    store.set("s", "a", "v2").unwrap();
    assert_eq!(store.get("s", "a").unwrap().as_deref(), Some("v2"));
    assert_eq!(store.get("s", "b").unwrap(), None);
    store.delete("s", "a").unwrap();
    store.delete("s", "a").unwrap();
    assert_eq!(store.get("s", "a").unwrap(), None);
    assert!(!format!("{store:?}").contains("v2"));
}

#[test]
fn path_prepends_dedups_and_drops_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let a = simple(tmp.path(), "a", &[]);
    let b = simple(tmp.path(), "b", &[]);
    let mut connectors = discover(tmp.path()).connectors;
    // A second connector sharing a's bin dir.
    let mut a2 = connectors[0].clone();
    a2.name = "a2".into();
    connectors.push(a2);

    let a_bin = a.join("bin").to_str().unwrap().to_owned();
    let b_bin = b.join("bin").to_str().unwrap().to_owned();
    let base = format!("/usr/bin::/bin:{b_bin}:/usr/bin");
    assert_eq!(
        path_with_connectors(&connectors, &base),
        format!("{a_bin}:{b_bin}:/usr/bin:/bin")
    );
    assert_eq!(path_with_connectors(&[], "/usr/bin:/bin"), "/usr/bin:/bin");
    assert_eq!(path_with_connectors(&connectors[..1], ""), a_bin);
}

/// Touches the real login Keychain; run with `cargo test -- --ignored`.
#[test]
#[ignore = "touches the real login keychain"]
fn keychain_round_trip() {
    let store = KeychainStore;
    let service = "com.berth.connector.test";
    let account = "BERTH_TEST_SECRET";
    store.delete(service, account).unwrap();
    assert_eq!(store.get(service, account).unwrap(), None);
    store.set(service, account, "s3cret-1").unwrap();
    store.set(service, account, "s3cret-2").unwrap();
    assert_eq!(
        store.get(service, account).unwrap().as_deref(),
        Some("s3cret-2")
    );
    store.delete(service, account).unwrap();
    assert_eq!(store.get(service, account).unwrap(), None);
    store.delete(service, account).unwrap();
}
