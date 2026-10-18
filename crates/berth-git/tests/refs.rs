//! Ref, branch, and worktree helpers against real temp repos.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use berth_git::{
    branch_exists, current_branch, default_base_ref, head_commit, is_repo, list_branches,
    list_worktrees, repo_root,
};
use common::{Repo, TestGit};

#[tokio::test]
async fn branches_and_head() {
    let repo = Repo::init().await;
    // Unborn: branch is known, no commit, not yet listed.
    assert_eq!(
        current_branch(&repo.git).await.unwrap().as_deref(),
        Some("main")
    );
    assert!(list_branches(&repo.git).await.unwrap().is_empty());
    assert!(!branch_exists(&repo.git, "main").await.unwrap());

    repo.write("a.txt", "a\n");
    let c1 = repo.commit_all("init").await;
    repo.git(&["branch", "feature/x"]).await;
    // A tag with a branch's name must not change listed names.
    repo.git(&["tag", "main"]).await;

    assert_eq!(
        list_branches(&repo.git).await.unwrap(),
        vec!["feature/x", "main"]
    );
    assert!(branch_exists(&repo.git, "feature/x").await.unwrap());
    assert!(!branch_exists(&repo.git, "nope").await.unwrap());
    assert!(!branch_exists(&repo.git, "").await.unwrap());

    repo.git(&["checkout", "-q", "--detach"]).await;
    assert_eq!(current_branch(&repo.git).await.unwrap(), None);
    assert_eq!(head_commit(&repo.git).await.unwrap(), Some(c1));
}

#[tokio::test]
async fn default_base_ref_fallbacks() {
    let main = Repo::init().await;
    main.write("a.txt", "a\n");
    main.commit_all("init").await;
    main.git(&["checkout", "-q", "-b", "work"]).await;
    assert_eq!(default_base_ref(&main.git).await.unwrap(), "main");

    let master = Repo::init_with_branch("master").await;
    master.write("a.txt", "a\n");
    master.commit_all("init").await;
    assert_eq!(default_base_ref(&master.git).await.unwrap(), "master");

    let trunk = Repo::init_with_branch("trunk").await;
    assert_eq!(default_base_ref(&trunk.git).await.unwrap(), "trunk");
    trunk.write("a.txt", "a\n");
    trunk.commit_all("init").await;
    trunk.git(&["checkout", "-q", "--detach"]).await;
    assert_eq!(default_base_ref(&trunk.git).await.unwrap(), "HEAD");

    // A clone has origin/HEAD -> origin/trunk, which wins over local branches.
    trunk.git(&["checkout", "-q", "trunk"]).await;
    let clone_path = trunk.sibling("clone");
    trunk
        .git(&["clone", "-q", ".", clone_path.to_str().unwrap()])
        .await;
    let clone = TestGit::at(&clone_path);
    common::run_ok(&clone, &["branch", "main"]).await;
    assert_eq!(default_base_ref(&clone).await.unwrap(), "origin/trunk");
}

#[tokio::test]
async fn list_worktrees_reports_branches_and_detached() {
    let repo = Repo::init().await;
    repo.write("a.txt", "a\n");
    let head = repo.commit_all("init").await;
    let feat = repo.sibling("wt feat");
    let det = repo.sibling("wt-det");
    repo.git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "feat",
        feat.to_str().unwrap(),
    ])
    .await;
    repo.git(&["worktree", "add", "-q", "--detach", det.to_str().unwrap()])
        .await;

    let wts = list_worktrees(&repo.git).await.unwrap();
    assert_eq!(wts.len(), 3, "{wts:#?}");
    assert_eq!(wts[0].path, repo.root);
    assert_eq!(wts[0].branch.as_deref(), Some("main"));
    assert_eq!(wts[0].head.as_deref(), Some(head.as_str()));
    let f = wts
        .iter()
        .find(|w| w.branch.as_deref() == Some("feat"))
        .unwrap();
    assert_eq!(f.path, feat.canonicalize().unwrap());
    let d = wts
        .iter()
        .find(|w| w.path == det.canonicalize().unwrap())
        .unwrap();
    assert_eq!(d.branch, None);
    assert_eq!(d.head.as_deref(), Some(head.as_str()));
    assert!(wts.iter().all(|w| !w.bare));

    // Same list from inside a linked worktree.
    let from_wt = list_worktrees(&TestGit::at(&det)).await.unwrap();
    assert_eq!(from_wt, wts);
}

#[tokio::test]
async fn is_repo_and_repo_root() {
    let repo = Repo::init().await;
    repo.write("sub/dir/f.txt", "f\n");
    assert!(is_repo(&repo.root).await);
    assert!(is_repo(&repo.root.join("sub/dir")).await);
    assert_eq!(
        repo_root(&repo.root.join("sub/dir")).await.unwrap(),
        repo.root
    );
    assert_eq!(repo_root(&repo.root).await.unwrap(), repo.root);

    let outside = tempfile::tempdir().unwrap();
    assert!(!is_repo(outside.path()).await);
    assert!(repo_root(outside.path()).await.is_err());
    assert!(!is_repo(&outside.path().join("missing")).await);
}
