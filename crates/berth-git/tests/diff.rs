//! `diff_trees` against real temp repos.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use berth_git::{
    DiffLine, FileDiff, FileStatus, GitError, LineKind, SnapshotIndex, diff_trees, snapshot,
};
use common::Repo;

fn find<'a>(files: &'a [FileDiff], path: &str) -> &'a FileDiff {
    files
        .iter()
        .find(|f| f.display_path() == path)
        .unwrap_or_else(|| panic!("{path} not in diff: {files:#?}"))
}

fn numbers(f: &FileDiff) -> Vec<(LineKind, Option<u32>, Option<u32>, &str)> {
    f.hunks
        .iter()
        .flat_map(|h| &h.lines)
        .map(|l: &DiffLine| (l.kind, l.old_line, l.new_line, l.text.as_str()))
        .collect()
}

fn seq(from: u32, to: u32) -> String {
    (from..=to).map(|n| format!("{n}\n")).collect()
}

#[tokio::test]
async fn diff_between_snapshots_reports_statuses_and_line_numbers() {
    let repo = Repo::init().await;
    repo.write("a.txt", seq(1, 10));
    repo.write("del.txt", "del\n");
    repo.write("ren.txt", seq(100, 130));
    repo.write("ren2.txt", seq(200, 230));
    repo.write("mode.sh", "#!/bin/sh\n");
    repo.write("with space.txt", "sp\n");
    repo.write("tab\tname.txt", "x\n");
    repo.write("café.txt", "é\n");
    repo.write("no_eol.txt", "x\ny");
    repo.write("img.bin", b"\x00\x01\x02old".as_slice());
    repo.commit_all("base").await;

    let idx = SnapshotIndex::GitDir;
    let start = snapshot(&repo.git, &idx).await.unwrap();

    repo.write("a.txt", seq(1, 10).replace("5\n", "five!\n") + "11\n");
    repo.write("new.txt", "n1\nn2\n");
    repo.remove("del.txt");
    std::fs::create_dir(repo.root.join("moved")).unwrap();
    std::fs::rename(repo.root.join("ren.txt"), repo.root.join("moved/ren.txt")).unwrap();
    repo.remove("ren2.txt");
    repo.write("ren2-renamed.txt", seq(200, 230).replace("215\n", "215!\n"));
    repo.chmod("mode.sh", 0o755);
    repo.write("bin.dat", b"\x00\x01\x02bin".as_slice());
    repo.write("img.bin", b"\x00\x01\x02new".as_slice());
    repo.write("with space.txt", "sp\nsp2\n");
    repo.write("tab\tname.txt", "x\ny\n");
    repo.write("café.txt", "é\nè\n");
    repo.write("empty.txt", "");
    repo.write("no_eol.txt", "x\ny\nz");

    let end = snapshot(&repo.git, &idx).await.unwrap();
    let diff = diff_trees(&repo.git, start.as_str(), end.as_str())
        .await
        .unwrap();
    let files = &diff.files;
    assert_eq!(files.len(), 13, "{files:#?}");

    let a = find(files, "a.txt");
    assert_eq!(a.status, FileStatus::Modified);
    let (l, c, r) = (LineKind::Added, LineKind::Context, LineKind::Removed);
    assert_eq!(
        numbers(a),
        vec![
            (c, Some(2), Some(2), "2"),
            (c, Some(3), Some(3), "3"),
            (c, Some(4), Some(4), "4"),
            (r, Some(5), None, "5"),
            (l, None, Some(5), "five!"),
            (c, Some(6), Some(6), "6"),
            (c, Some(7), Some(7), "7"),
            (c, Some(8), Some(8), "8"),
            (c, Some(9), Some(9), "9"),
            (c, Some(10), Some(10), "10"),
            (l, None, Some(11), "11"),
        ]
    );
    assert_eq!((a.additions(), a.deletions()), (2, 1));
    assert_eq!(a.old_oid.as_ref().map(String::len), Some(40));

    let new = find(files, "new.txt");
    assert_eq!(new.status, FileStatus::Added);
    assert_eq!(new.old_path, None);
    assert_eq!(new.new_mode.as_deref(), Some("100644"));
    assert_eq!(
        numbers(new),
        vec![(l, None, Some(1), "n1"), (l, None, Some(2), "n2")]
    );

    let del = find(files, "del.txt");
    assert_eq!(del.status, FileStatus::Deleted);
    assert_eq!(del.new_path, None);
    assert_eq!(numbers(del), vec![(r, Some(1), None, "del")]);

    let ren = find(files, "moved/ren.txt");
    assert_eq!(ren.status, FileStatus::Renamed { similarity: 100 });
    assert_eq!(ren.old_path.as_deref(), Some("ren.txt"));
    assert!(ren.hunks.is_empty());

    let ren2 = find(files, "ren2-renamed.txt");
    assert!(matches!(ren2.status, FileStatus::Renamed { similarity } if similarity < 100));
    assert_eq!(ren2.old_path.as_deref(), Some("ren2.txt"));
    assert!(numbers(ren2).contains(&(r, Some(16), None, "215")));
    assert!(numbers(ren2).contains(&(l, None, Some(16), "215!")));

    let mode = find(files, "mode.sh");
    assert_eq!(mode.status, FileStatus::ModeChanged);
    assert_eq!(mode.old_mode.as_deref(), Some("100644"));
    assert_eq!(mode.new_mode.as_deref(), Some("100755"));

    let bin = find(files, "bin.dat");
    assert_eq!(bin.status, FileStatus::Added);
    assert!(bin.binary);
    assert!(bin.hunks.is_empty());
    let img = find(files, "img.bin");
    assert_eq!(img.status, FileStatus::Modified);
    assert!(img.binary);

    let space = find(files, "with space.txt");
    assert_eq!(space.old_path.as_deref(), Some("with space.txt"));
    assert_eq!(numbers(space).last(), Some(&(l, None, Some(2), "sp2")));

    let tab = find(files, "tab\tname.txt");
    assert_eq!(tab.status, FileStatus::Modified);
    assert_eq!(numbers(tab).last(), Some(&(l, None, Some(2), "y")));

    let cafe = find(files, "café.txt");
    assert_eq!(numbers(cafe).last(), Some(&(l, None, Some(2), "è")));

    let empty = find(files, "empty.txt");
    assert_eq!(empty.status, FileStatus::Added);
    assert!(empty.hunks.is_empty());
    assert!(!empty.binary);

    let eol = find(files, "no_eol.txt");
    let lines: Vec<_> = eol.hunks[0].lines.iter().collect();
    assert_eq!(lines.len(), 4);
    assert!(lines[1].no_newline_at_eof && lines[1].kind == r && lines[1].text == "y");
    assert!(!lines[2].no_newline_at_eof && lines[2].kind == l && lines[2].text == "y");
    assert!(lines[3].no_newline_at_eof && lines[3].text == "z");

    let stats = diff.stats();
    assert_eq!(stats.files, 13);
    assert_eq!(
        stats.additions,
        files.iter().map(FileDiff::additions).sum::<usize>()
    );
}

#[tokio::test]
async fn identical_trees_give_empty_diff() {
    let repo = Repo::init().await;
    repo.write("a.txt", "a\n");
    repo.commit_all("init").await;
    let snap = snapshot(&repo.git, &SnapshotIndex::GitDir).await.unwrap();
    let diff = diff_trees(&repo.git, "HEAD", snap.as_str()).await.unwrap();
    assert!(diff.is_empty());
    assert_eq!(diff.stats().files, 0);
}

#[tokio::test]
async fn diff_trees_accepts_commits_and_rejects_options() {
    let repo = Repo::init().await;
    repo.write("a.txt", "1\n");
    repo.commit_all("one").await;
    repo.write("a.txt", "2\n");
    repo.commit_all("two").await;

    let diff = diff_trees(&repo.git, "HEAD~1", "HEAD").await.unwrap();
    assert_eq!(diff.files.len(), 1);
    assert_eq!(diff.stats().additions, 1);
    assert_eq!(diff.stats().deletions, 1);

    assert!(matches!(
        diff_trees(&repo.git, "--output=/tmp/x", "HEAD").await,
        Err(GitError::InvalidRev(_))
    ));
    assert!(matches!(
        diff_trees(&repo.git, "HEAD", "no-such-rev").await,
        Err(GitError::Failed { .. })
    ));
}
