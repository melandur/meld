from meld.ailens.models import GitFileChange
from meld.ailens.session import FileEntry


def tree(paths, **kwargs):
    from meld.gitcompare import build_file_tree

    entries = [
        FileEntry(GitFileChange(kwargs.get("status", "M"), p), 1, 0) for p in paths
    ]
    return build_file_tree(entries)


def dump(rows, depth=0):
    lines = []
    for row in rows:
        lines.append("  " * depth + row.name + ("/" if row.entry is None else ""))
        lines += dump(row.children, depth + 1)
    return lines


def test_folders_first_and_single_folder_chains_compacted():
    rows = tree(
        [
            "README.md",
            ".github/workflows/ci.yml",
            "src/a/b/deep.py",
            "src/top.py",
            "agent/index.md",
        ]
    )
    assert dump(rows) == [
        ".github/workflows/",
        "  ci.yml",
        "agent/",
        "  index.md",
        "src/",
        "  a/b/",
        "    deep.py",
        "  top.py",
        "README.md",
    ]


def test_same_path_listed_twice():
    from meld.gitcompare import build_file_tree

    entries = [
        FileEntry(GitFileChange("D", "gone.txt"), 0, 1),
        FileEntry(GitFileChange("A", "gone.txt", untracked=True), 1, 0),
    ]
    names = [row.name for row in build_file_tree(entries)]
    assert sorted(names) == ["gone.txt", "gone.txt (untracked)"]
