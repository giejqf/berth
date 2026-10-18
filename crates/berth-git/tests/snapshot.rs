//! Snapshots against real temp repos (AGENTS.md §7).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use berth_git::{
    FileStatus, GITDIR_INDEX_NAME, SnapshotIndex, diff_trees, head_commit, merge_base,
    resolve_tree, snapshot,
};
use common::{Repo, TestGit, run_ok, status_of, tree_files};

fn state_index(repo: &Repo) -> SnapshotIndex {
    let state = repo.sibling("slot-state");
    std::fs::create_dir_all(&state).unwrap();
    SnapshotIndex::Path(state.join("snap.idx").to_string_lossy().into_owned())
}

#[tokio::test]
async fn repeated_snapshots_are_stable_and_match_head() {
    let repo = Repo::init().await;
    repo.write("a.txt", "a\n");
    repo.write("dir/b.txt", "b\n");
    repo.commit_all("init").await;

    let idx = state_index(&repo);
    let first = snapshot(&repo.git, &idx).await.unwrap();
    let second = snapshot(&repo.git, &idx).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(first, resolve_tree(&repo.git, "HEAD").await.unwrap());

    // The index file is kept for stat caching.
    let SnapshotIndex::Path(p) = &idx else {
        unreachable!()
    };
    assert!(std::path::Path::new(p).exists());

    // A change produces a new tree; reverting it returns to the original.
    repo.write("a.txt", "changed\n");
    let changed = snapshot(&repo.git, &idx).await.unwrap();
    assert_ne!(changed, first);
    repo.write("a.txt", "a\n");
    assert_eq!(snapshot(&repo.git, &idx).await.unwrap(), first);
}

#[tokio::test]
async fn snapshot_leaves_real_index_untouched() {
    let repo = Repo::init().await;
    repo.write("staged.txt", "1\n");
    repo.write("unstaged.txt", "1\n");
    repo.write("deleted.txt", "1\n");
    repo.commit_all("init").await;

    // Clean staging area stays clean.
    repo.write("unstaged.txt", "2\n");
    repo.write("untracked.txt", "new\n");
    let idx = state_index(&repo);
    snapshot(&repo.git, &idx).await.unwrap();
    assert_eq!(
        status_of(&repo.git, &["diff", "--cached", "--quiet"]).await,
        0
    );
    assert_eq!(
        repo.git(&["ls-files"]).await,
        "deleted.txt\nstaged.txt\nunstaged.txt"
    );

    // A partially staged state is preserved exactly.
    repo.write("staged.txt", "2\n");
    repo.git(&["add", "staged.txt"]).await;
    repo.remove("deleted.txt");
    let status_before = repo.git(&["status", "--porcelain=v1"]).await;
    let stage_before = repo.git(&["ls-files", "--stage"]).await;
    let index_file = repo.root.join(".git/index");
    let bytes_before = std::fs::read(&index_file).unwrap();

    let snap = snapshot(&repo.git, &idx).await.unwrap();

    assert_eq!(std::fs::read(&index_file).unwrap(), bytes_before);
    assert_eq!(repo.git(&["ls-files", "--stage"]).await, stage_before);
    assert_eq!(repo.git(&["status", "--porcelain=v1"]).await, status_before);
    assert_eq!(
        repo.git(&["diff", "--cached", "--name-only"]).await,
        "staged.txt"
    );

    // The snapshot sees staged, unstaged, deleted and untracked changes alike.
    let diff = diff_trees(&repo.git, "HEAD", snap.as_str()).await.unwrap();
    let mut got: Vec<(String, FileStatus)> = diff
        .files
        .iter()
        .map(|f| (f.display_path().to_owned(), f.status))
        .collect();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        got,
        vec![
            ("deleted.txt".to_owned(), FileStatus::Deleted),
            ("staged.txt".to_owned(), FileStatus::Modified),
            ("unstaged.txt".to_owned(), FileStatus::Modified),
            ("untracked.txt".to_owned(), FileStatus::Added),
        ]
    );
}

#[tokio::test]
async fn snapshot_excludes_ignored_but_keeps_tracked_ignored_files() {
    let repo = Repo::init().await;
    repo.write(".gitignore", "*.log\nbuild/\n");
    repo.write("src.txt", "s\n");
    repo.write("keep.log", "tracked despite ignore\n");
    repo.git(&["add", ".gitignore", "src.txt"]).await;
    repo.git(&["add", "-f", "keep.log"]).await;
    repo.git(&["commit", "-q", "-m", "init"]).await;

    repo.write("debug.log", "noise\n");
    repo.write("build/out.o", "obj\n");
    repo.write("new.txt", "n\n");

    let idx = SnapshotIndex::GitDir;
    let tree = snapshot(&repo.git, &idx).await.unwrap();
    let files = tree_files(&repo.git, tree.as_str()).await;
    assert_eq!(files, vec![".gitignore", "keep.log", "new.txt", "src.txt"]);

    // Edits to a tracked-but-ignored file are captured too.
    repo.remove("new.txt");
    assert_eq!(
        snapshot(&repo.git, &idx).await.unwrap(),
        resolve_tree(&repo.git, "HEAD").await.unwrap()
    );
    repo.write("keep.log", "edited\n");
    let edited = snapshot(&repo.git, &idx).await.unwrap();
    let diff = diff_trees(&repo.git, "HEAD", edited.as_str())
        .await
        .unwrap();
    assert_eq!(diff.files.len(), 1);
    assert_eq!(diff.files[0].display_path(), "keep.log");
}

#[tokio::test]
async fn gitdir_index_in_main_checkout_and_linked_worktree() {
    let repo = Repo::init().await;
    repo.write("a.txt", "a\n");
    repo.write("sub/b.txt", "b\n");
    repo.commit_all("init").await;
    let head_tree = resolve_tree(&repo.git, "HEAD").await.unwrap();

    let main_idx = SnapshotIndex::GitDir.resolve(&repo.git).await.unwrap();
    let expected = repo.root.join(".git").join(GITDIR_INDEX_NAME);
    assert_eq!(main_idx, expected.to_string_lossy());
    assert_eq!(
        snapshot(&repo.git, &SnapshotIndex::GitDir).await.unwrap(),
        head_tree
    );
    assert!(expected.exists());

    // Relative `--git-path` output from a subdirectory is made absolute.
    let sub = TestGit::at(repo.root.join("sub"));
    assert_eq!(SnapshotIndex::GitDir.resolve(&sub).await.unwrap(), main_idx);

    let wt_path = repo.sibling("wt");
    repo.git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "feature",
        wt_path.to_str().unwrap(),
    ])
    .await;
    let wt = TestGit::at(wt_path.canonicalize().unwrap());
    let wt_idx = SnapshotIndex::GitDir.resolve(&wt).await.unwrap();
    assert_eq!(
        wt_idx,
        repo.root
            .join(".git/worktrees/wt")
            .join(GITDIR_INDEX_NAME)
            .to_string_lossy()
    );

    assert_eq!(
        snapshot(&wt, &SnapshotIndex::GitDir).await.unwrap(),
        head_tree
    );
    std::fs::write(wt_path.join("a.txt"), "a\nmore\n").unwrap();
    let wt_snap = snapshot(&wt, &SnapshotIndex::GitDir).await.unwrap();
    assert_ne!(wt_snap, head_tree);
    let diff = diff_trees(&wt, head_tree.as_str(), wt_snap.as_str())
        .await
        .unwrap();
    assert_eq!(diff.files.len(), 1);
    assert_eq!(diff.files[0].display_path(), "a.txt");
    assert_eq!(diff.files[0].hunks[0].lines[1].new_line, Some(2));

    // The main checkout is unaffected by the worktree's edits.
    assert_eq!(
        snapshot(&repo.git, &SnapshotIndex::GitDir).await.unwrap(),
        head_tree
    );
    assert_eq!(status_of(&wt, &["diff", "--cached", "--quiet"]).await, 0);
}

#[tokio::test]
async fn snapshot_with_unborn_head() {
    let repo = Repo::init().await;
    assert_eq!(head_commit(&repo.git).await.unwrap(), None);
    repo.write("first.txt", "1\n");
    let tree = snapshot(&repo.git, &SnapshotIndex::GitDir).await.unwrap();
    assert_eq!(
        tree_files(&repo.git, tree.as_str()).await,
        vec!["first.txt"]
    );
    // Nothing was staged in the real index.
    assert_eq!(repo.git(&["ls-files"]).await, "");
}

#[tokio::test]
async fn concurrent_snapshots_share_an_index() {
    let repo = Repo::init().await;
    for i in 0..50 {
        repo.write(&format!("f{i}.txt"), format!("{i}\n"));
    }
    repo.commit_all("init").await;
    repo.write("f0.txt", "changed\n");
    let idx = state_index(&repo);
    let (a, b, c) = tokio::join!(
        snapshot(&repo.git, &idx),
        snapshot(&repo.git, &idx),
        snapshot(&repo.git, &idx)
    );
    let a = a.unwrap();
    assert_eq!(a, b.unwrap());
    assert_eq!(a, c.unwrap());
}

#[tokio::test]
async fn resolve_tree_merge_base_and_head_commit() {
    let repo = Repo::init().await;
    repo.write("a.txt", "a\n");
    let base = repo.commit_all("base").await;
    assert_eq!(
        head_commit(&repo.git).await.unwrap().as_deref(),
        Some(base.as_str())
    );

    repo.git(&["checkout", "-q", "-b", "feature"]).await;
    repo.write("f.txt", "f\n");
    let feature = repo.commit_all("feature").await;
    repo.git(&["checkout", "-q", "main"]).await;
    repo.write("m.txt", "m\n");
    repo.commit_all("main").await;

    assert_eq!(
        merge_base(&repo.git, "main", "feature").await.unwrap(),
        base
    );
    assert_eq!(merge_base(&repo.git, "main", &feature).await.unwrap(), base);

    let tree = resolve_tree(&repo.git, "feature").await.unwrap();
    assert_eq!(
        tree.as_str(),
        run_ok(&repo.git, &["rev-parse", "feature^{tree}"]).await
    );
    // A tree id resolves to itself.
    assert_eq!(resolve_tree(&repo.git, tree.as_str()).await.unwrap(), tree);
    assert!(resolve_tree(&repo.git, "no-such-ref").await.is_err());
    assert!(matches!(
        resolve_tree(&repo.git, "--all").await,
        Err(berth_git::GitError::InvalidRev(_))
    ));
}
