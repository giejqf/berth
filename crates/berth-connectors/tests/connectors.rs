//! Connector discovery, skills, origins and secrets against temp-dir fixtures.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use berth_connectors::{
    Connector, ConnectorError, KeychainStore, MemoryStore, OriginFields, OriginItem, OriginSource,
    SecretStore, default_connectors_dir, discover, path_with_connectors, secret_env,
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
            title: "linear".into(),
            description: "Linear issues".into(),
            dir: dir.clone(),
            binary: dir.join("bin/linear-cli"),
            secrets: vec!["LINEAR_API_KEY".into()],
            optional_secrets: Vec::new(),
            skill_md: Some(dir.join("SKILL.md")),
            origin: Some(OriginSource {
                kind: "linear".into(),
                list: vec!["issues".into(), "list".into(), "--json".into()],
                items: None,
                fields: OriginFields::default(),
            }),
            builtin: false,
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
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&checkout)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    git(&["init", "--quiet"]);
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
    // Git ignores it, once: it never shows in a diff or a commit.
    assert_eq!(git(&["status", "--porcelain", "--untracked-files=all"]), "");
    let exclude = fs::read_to_string(checkout.join(".git/info/exclude")).unwrap();
    assert_eq!(
        exclude.matches("/.claude/skills/linear/SKILL.md").count(),
        1
    );
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
    assert!(err.to_string().contains("not a JSON array"), "{err:?}");

    let dir = connector_dir(
        tmp.path(),
        "b",
        "name = \"b\"\nbinary = \"bin/b\"\n[origin]\nkind = \"b\"\n",
        Some(("b", "#!/bin/sh\necho 'not json'\n")),
    );
    let err = Connector::load(&dir)
        .unwrap()
        .list_origins(&[])
        .await
        .unwrap_err();
    assert!(matches!(err, ConnectorError::Json(_)), "{err:?}");
}

/// PostHog's shape: `{"results": [{"id", "name", "description"}], ...}`.
const POSTHOG_TOML: &str = r#"
name = "posthog"
title = "PostHog"
binary = "bin/posthog-cli"
secrets = ["POSTHOG_CLI_API_KEY"]
optional_secrets = ["POSTHOG_CLI_HOST"]

[origin]
kind = "posthog"
list = ["issues"]
items = "results"
fields = { ref = "id", title = "name", body = "description", url = "links.0.href" }
"#;

const POSTHOG_BIN: &str = r#"#!/bin/sh
echo '{"results":[{"id":"0198-a","name":"TypeError","description":"x is undefined","links":[{"href":"https://ph/1"}]},{"id":42,"name":"Panic","description":null}],"hasMore":false}'
"#;

#[tokio::test]
async fn list_origins_maps_items_and_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = connector_dir(
        tmp.path(),
        "posthog",
        POSTHOG_TOML,
        Some(("posthog-cli", POSTHOG_BIN)),
    );
    let c = Connector::load(&dir).unwrap();
    assert_eq!(c.title, "PostHog");
    assert_eq!(
        c.list_origins(&[]).await.unwrap(),
        vec![
            OriginItem {
                r#ref: "0198-a".into(),
                title: "TypeError".into(),
                body: Some("x is undefined".into()),
                url: Some("https://ph/1".into()),
            },
            OriginItem {
                r#ref: "42".into(),
                title: "Panic".into(),
                body: None,
                url: None,
            },
        ]
    );
}

#[tokio::test]
async fn list_origins_reports_unmapped_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let toml = POSTHOG_TOML.replace("ref = \"id\"", "ref = \"uuid\"");
    let dir = connector_dir(
        tmp.path(),
        "posthog",
        &toml,
        Some(("posthog-cli", POSTHOG_BIN)),
    );
    let err = Connector::load(&dir)
        .unwrap()
        .list_origins(&[])
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("item 0 has no `ref` (read from `uuid`)"),
        "{err}"
    );

    let toml = POSTHOG_TOML.replace("items = \"results\"", "items = \"data.rows\"");
    let dir = connector_dir(
        tmp.path(),
        "posthog2",
        &toml.replace("name = \"posthog\"", "name = \"posthog2\""),
        Some(("posthog-cli", POSTHOG_BIN)),
    );
    let err = Connector::load(&dir)
        .unwrap()
        .list_origins(&[])
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("output has no `data.rows`"),
        "{err}"
    );
}

#[test]
fn origin_fields_are_strict() {
    let tmp = tempfile::tempdir().unwrap();
    let toml = POSTHOG_TOML.replace("url = ", "link = ");
    connector_dir(
        tmp.path(),
        "posthog",
        &toml,
        Some(("posthog-cli", POSTHOG_BIN)),
    );
    let (_, err) = only_error(&tmp);
    assert!(matches!(err, ConnectorError::Toml { .. }), "{err:?}");
}

#[test]
fn title_defaults_to_name() {
    let tmp = tempfile::tempdir().unwrap();
    let c = Connector::load(&linear(tmp.path())).unwrap();
    assert_eq!(c.title, "linear");
    assert!(!c.builtin);
}

#[test]
fn user_connectors_replace_built_in_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let shipped = tmp.path().join("shipped");
    let user = tmp.path().join("user");
    linear(&shipped);
    simple(&shipped, "posthog", &[]);
    connector_dir(&shipped, "broken", "name = \"broken\"\n", None);
    simple(&user, "linear", &["MY_KEY"]);

    let d = berth_connectors::discover_layers(Some(&shipped), &user);
    let names: Vec<_> = d
        .connectors
        .iter()
        .map(|c| (c.name.as_str(), c.builtin))
        .collect();
    assert_eq!(names, [("linear", false), ("posthog", true)]);
    assert_eq!(d.connectors[0].secrets, ["MY_KEY"]);
    assert_eq!(d.errors.len(), 1, "{:?}", d.errors);

    let d = berth_connectors::discover_layers(None, &user);
    assert_eq!(d.connectors.len(), 1);
}

#[test]
fn optional_secrets_are_passed_but_never_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = connector_dir(
        tmp.path(),
        "posthog",
        POSTHOG_TOML,
        Some(("posthog-cli", POSTHOG_BIN)),
    );
    let connectors = vec![Connector::load(&dir).unwrap()];
    let store = MemoryStore::new();
    let env = secret_env(&connectors, &store).unwrap();
    assert!(env.vars.is_empty());
    assert_eq!(
        env.missing,
        vec![("posthog".to_owned(), "POSTHOG_CLI_API_KEY".to_owned())]
    );

    store
        .set(
            "com.berth.connector.posthog",
            "POSTHOG_CLI_HOST",
            "https://eu.posthog.com",
        )
        .unwrap();
    let env = secret_env(&connectors, &store).unwrap();
    assert_eq!(
        env.vars,
        vec![(
            "POSTHOG_CLI_HOST".to_owned(),
            "https://eu.posthog.com".to_owned()
        )]
    );
}

#[test]
fn optional_secret_names_are_validated() {
    let tmp = tempfile::tempdir().unwrap();
    connector_dir(
        tmp.path(),
        "a",
        "name = \"a\"\nbinary = \"bin/a\"\nsecrets = [\"K\"]\noptional_secrets = [\"K\"]\n",
        Some(("a", "#!/bin/sh\n")),
    );
    let (_, err) = only_error(&tmp);
    assert!(err.to_string().contains("listed twice"), "{err}");
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

/// The built-in connectors in `connectors/` load (with a stand-in binary, since
/// `bin/` is fetched) and map their CLIs' output shapes.
#[test]
fn shipped_connectors_are_valid() {
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../connectors");
    let tmp = tempfile::tempdir().unwrap();
    for (name, bin) in [("linear", "linear-cli"), ("posthog", "posthog-cli")] {
        let dir = tmp.path().join(name);
        fs::create_dir_all(&dir).unwrap();
        for file in ["connector.toml", "SKILL.md"] {
            fs::copy(shipped.join(name).join(file), dir.join(file)).unwrap();
        }
        write_exe(&dir.join("bin").join(bin), "#!/bin/sh\n");
    }
    let d = discover(tmp.path());
    assert!(d.errors.is_empty(), "{:?}", d.errors);
    let [linear, posthog] = &d.connectors[..] else {
        panic!("{:?}", d.connectors);
    };
    assert_eq!(
        (linear.title.as_str(), posthog.title.as_str()),
        ("Linear", "PostHog")
    );
    assert!(linear.skill_md.is_some() && posthog.skill_md.is_some());
    assert_eq!(linear.secrets, ["LINEAR_API_KEY"]);
    assert_eq!(posthog.optional_secrets, ["POSTHOG_CLI_HOST"]);

    // linear-cli `api query` prints the response; the aliased nodes are in it.
    let response = serde_json::json!({"data": {"issues": {"nodes": [
        {"ref": "ENG-1", "title": "Fix login", "body": null, "url": "https://linear.app/a/issue/ENG-1"}
    ]}}});
    let items = linear.origin.as_ref().unwrap().map(&response).unwrap();
    assert_eq!(items[0].r#ref, "ENG-1");
    assert_eq!(items[0].body, None);

    // posthog-cli `api call --json query-error-tracking-issues-list`.
    let page = serde_json::json!({
        "results": [{"id": "01963e1c-aaaa", "name": "TypeError", "description": "x is undefined",
                     "status": "active", "aggregations": {"occurrences": 12},
                     "_posthogUrl": "https://us.posthog.com/project/1/error_tracking/01963e1c-aaaa"}],
        "hasMore": false,
        "_posthogUrl": "https://us.posthog.com/project/1/error_tracking"
    });
    let items = posthog.origin.as_ref().unwrap().map(&page).unwrap();
    assert_eq!(
        items,
        [OriginItem {
            r#ref: "01963e1c-aaaa".into(),
            title: "TypeError".into(),
            body: Some("x is undefined".into()),
            url: Some("https://us.posthog.com/project/1/error_tracking/01963e1c-aaaa".into()),
        }]
    );
}
