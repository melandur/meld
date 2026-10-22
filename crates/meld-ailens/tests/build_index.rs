//! Port of test/ailens/test_build_index.py (itself ported from AI Lens
//! src/build/__tests__/buildIndex.test.ts, plus edge cases)

use std::path::{Path, PathBuf};

use meld_ailens::build_file::{BuildHeader, HeaderCommit};
use meld_ailens::build_index::{
    find_builds, is_build_file_name, read_build_headers, resolve_build_folder,
};

fn header(file_name: &str, created: Option<&str>, commits: Vec<HeaderCommit>) -> BuildHeader {
    BuildHeader {
        created: created.map(str::to_string),
        commits,
        ..BuildHeader::new(file_name)
    }
}

fn headers() -> Vec<BuildHeader> {
    vec![
        header(
            "a_build.md",
            Some("2026-01-01T00:00:00Z"),
            vec![HeaderCommit::new("x1", Some("p1"))],
        ),
        header(
            "b_build.md",
            Some("2026-03-01T00:00:00Z"),
            vec![
                HeaderCommit::new("x1", Some("p1")),
                HeaderCommit::new("x2", None),
            ],
        ),
    ]
}

fn resolve(root: &str, folder: &str) -> PathBuf {
    resolve_build_folder(Path::new(root), folder).unwrap()
}

fn refused(root: &Path, folder: &str) {
    let err = resolve_build_folder(root, folder).unwrap_err();
    assert!(
        err.to_string().contains("inside the repository"),
        "{folder:?}: {err}"
    );
}

// -- Ported --

#[test]
fn find_builds_by_sha_newest_first() {
    assert_eq!(
        find_builds(&headers(), "x1", None),
        ["b_build.md", "a_build.md"]
    );
    assert_eq!(find_builds(&headers(), "x2", None), ["b_build.md"]);
}

#[test]
fn find_builds_by_patch_id_for_new_sha() {
    assert_eq!(
        find_builds(&headers(), "rebased", Some("p1")),
        ["b_build.md", "a_build.md"]
    );
    assert!(find_builds(&headers(), "rebased", Some("other")).is_empty());
}

#[test]
fn is_build_file_name_accepts_only_plain_names() {
    assert!(is_build_file_name("a1b2c3d_9f8e7d6_build.md"));
    assert!(!is_build_file_name("../secrets_build.md"));
    assert!(!is_build_file_name("sub/x_build.md"));
    assert!(!is_build_file_name("index.md"));
}

#[test]
fn resolve_build_folder_keeps_it_inside() {
    // From generateBuild.test.ts
    assert_eq!(resolve("/repo", "agent"), Path::new("/repo/agent"));
    assert_eq!(
        resolve("/repo", " docs/agent "),
        Path::new("/repo/docs/agent")
    );
    refused(Path::new("/repo"), "../elsewhere");
    refused(Path::new("/repo"), "/tmp/agent");
}

// -- find_builds --

#[test]
fn find_builds_none_patch_id_does_not_match_missing_ones() {
    // A header commit without a patch ID must not match a lookup without one
    assert!(find_builds(&headers(), "nope", None).is_empty());
}

#[test]
fn find_builds_same_date_by_name_and_undated_last() {
    let headers = [
        header(
            "z_build.md",
            Some("2026-01-01T00:00:00Z"),
            vec![HeaderCommit::new("x", None)],
        ),
        header("m_build.md", None, vec![HeaderCommit::new("x", None)]),
        header(
            "a_build.md",
            Some("2026-01-01T00:00:00Z"),
            vec![HeaderCommit::new("x", None)],
        ),
    ];
    assert_eq!(
        find_builds(&headers, "x", None),
        ["a_build.md", "z_build.md", "m_build.md"]
    );
}

// -- is_build_file_name --

#[test]
fn build_file_names() {
    for name in [
        "x_build.md",
        "a b_build.md",
        "été_build.md",
        "x..y_build.md",
    ] {
        assert!(is_build_file_name(name), "{name:?}");
    }
}

#[test]
fn not_build_file_names() {
    for name in [
        "_build.md",
        ".hidden_build.md",
        "..\\x_build.md",
        "sub\\x_build.md",
        "/abs_build.md",
        "x_build.md\n",
        "x_build.md.tmp",
        "x_build.mdx",
        "x_BUILD.md",
        "",
    ] {
        assert!(!is_build_file_name(name), "{name:?}");
    }
}

// -- resolve_build_folder --

#[test]
fn resolve_build_folder_defaults_and_normalizes() {
    assert_eq!(resolve("/repo", ""), Path::new("/repo/agent"));
    assert_eq!(resolve("/repo", "   "), Path::new("/repo/agent"));
    assert_eq!(resolve("/repo", "./a//b/"), Path::new("/repo/a/b"));
    assert_eq!(resolve("/repo", "a/../b"), Path::new("/repo/b"));
    assert_eq!(resolve("/repo/", "agent"), Path::new("/repo/agent"));
    assert_eq!(resolve("/repo", "."), Path::new("/repo"));
    assert_eq!(resolve("/repo", "..foo"), Path::new("/repo/..foo"));
}

#[test]
fn resolve_build_folder_refuses_traversal() {
    for folder in [
        "..",
        "../",
        "../..",
        "a/../../b",
        "a/../..",
        "./../x",
        "/",
        "/etc",
        "//etc",
        " /etc ",
    ] {
        refused(Path::new("/repo"), folder);
    }
}

#[cfg(unix)]
#[test]
fn resolve_build_folder_refuses_symlinks_out() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&repo).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, repo.join("out")).unwrap();
    std::fs::create_dir(repo.join("inner")).unwrap();
    std::os::unix::fs::symlink(repo.join("inner"), repo.join("in")).unwrap();

    refused(&repo, "out");
    refused(&repo, "out/agent");
    // A symlink that stays inside is fine
    assert_eq!(
        resolve_build_folder(&repo, "in/agent").unwrap(),
        repo.join("in").join("agent")
    );
}

#[cfg(unix)]
#[test]
fn resolve_build_folder_through_symlinked_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_eq!(
        resolve_build_folder(&link, "agent").unwrap(),
        link.join("agent")
    );
}

// -- read_build_headers --

#[test]
fn read_build_headers_missing_folder() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(
        read_build_headers(&tmp.path().join("nope"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn read_build_headers_reads_builds_in_name_order() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(dir.join("b_build.md"), "---\ntitle: \"B\"\n---\n").unwrap();
    std::fs::write(dir.join("a_build.md"), "---\ntitle: \"A\"\n---\n").unwrap();
    std::fs::write(dir.join("index.md"), "---\ntitle: \"index\"\n---\n").unwrap();
    std::fs::write(dir.join("notes.md"), "x").unwrap();
    // Not UTF-8, no frontmatter, a folder with a build's name: all tolerated
    std::fs::write(dir.join("c_build.md"), b"---\ntitle: \"\xff\xfe\"\n---\n").unwrap();
    std::fs::write(dir.join("d_build.md"), "# hand-written").unwrap();
    std::fs::create_dir(dir.join("e_build.md")).unwrap();

    let found: Vec<(String, Option<String>)> = read_build_headers(dir)
        .unwrap()
        .into_iter()
        .map(|h| (h.file_name, h.title))
        .collect();
    assert_eq!(
        found,
        [
            ("a_build.md".to_string(), Some("A".to_string())),
            ("b_build.md".into(), Some("B".into())),
            ("c_build.md".into(), Some("\u{fffd}\u{fffd}".into())),
            ("d_build.md".into(), None),
        ]
    );
}

#[test]
fn read_build_headers_folder_is_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("agent");
    std::fs::write(&path, "not a folder").unwrap();
    let err = read_build_headers(&path).unwrap_err();
    assert!(
        err.to_string().contains("Could not read the build folder"),
        "{err}"
    );
}

#[cfg(unix)]
#[test]
fn read_build_headers_skips_unreadable_files() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: geteuid has no preconditions
    if unsafe { libc_geteuid() } == 0 {
        return; // root reads anything
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(dir.join("a_build.md"), "---\ntitle: \"A\"\n---\n").unwrap();
    let locked = dir.join("b_build.md");
    std::fs::write(&locked, "---\ntitle: \"B\"\n---\n").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o0)).unwrap();
    let names: Vec<String> = read_build_headers(dir)
        .unwrap()
        .into_iter()
        .map(|h| h.file_name)
        .collect();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(names, ["a_build.md"]);
}

#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
}
