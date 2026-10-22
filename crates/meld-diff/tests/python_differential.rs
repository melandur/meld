//! Differential tests against outputs of the Python implementation, stored
//! by `tests/data/generate.py` (randomised but seeded inputs).

use meld_diff::{
    Chunk, ChunkLocation, Differ, DiffsChanged, MatchingBlock, MergedChunk, MyersSequenceMatcher,
    PatchSide, Tag, UnifiedDiffOptions, make_patch, merge_2_files, merge_3_files, unified_diff,
};
use serde_json::Value;

fn fixtures() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/python_fixtures.json"
    );
    let data = std::fs::read_to_string(path).expect("fixture file");
    serde_json::from_str(&data).expect("valid JSON")
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_owned())
        .collect()
}

fn usize_of(v: &Value) -> usize {
    v.as_u64().unwrap() as usize
}

fn opt_usize(v: &Value) -> Option<usize> {
    v.as_u64().map(|x| x as usize)
}

fn chunk(v: &Value) -> Option<Chunk> {
    let a = v.as_array()?;
    Some(Chunk::new(
        Tag::from_name(a[0].as_str().unwrap()).unwrap(),
        usize_of(&a[1]),
        usize_of(&a[2]),
        usize_of(&a[3]),
        usize_of(&a[4]),
    ))
}

/// Chunks of removed/modified sets may have negative coordinates in
/// Python; Rust reports them clamped at zero.
fn clamped_chunk(v: &Value) -> Option<Chunk> {
    let a = v.as_array()?;
    let z = |v: &Value| v.as_i64().unwrap().max(0) as usize;
    Some(Chunk::new(
        Tag::from_name(a[0].as_str().unwrap()).unwrap(),
        z(&a[1]),
        z(&a[2]),
        z(&a[3]),
        z(&a[4]),
    ))
}

fn clamped_list(v: &Value) -> Vec<MergedChunk> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|m| [clamped_chunk(&m[0]), clamped_chunk(&m[1])])
        .collect()
}

fn chunks(v: &Value) -> Vec<Chunk> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|c| chunk(c).unwrap())
        .collect()
}

fn blocks(v: &Value) -> Vec<MatchingBlock> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|b| MatchingBlock::new(usize_of(&b[0]), usize_of(&b[1]), usize_of(&b[2])))
        .collect()
}

fn merged(v: &Value) -> MergedChunk {
    [chunk(&v[0]), chunk(&v[1])]
}

fn merged_list(v: &Value) -> Vec<MergedChunk> {
    v.as_array().unwrap().iter().map(merged).collect()
}

fn sorted_by_json(mut v: Vec<MergedChunk>) -> Vec<MergedChunk> {
    // Python sorted these by their JSON encoding; we just need a canonical order.
    v.sort_by_key(|m| format!("{m:?}"));
    v
}

#[test]
fn matchers_match_python() {
    let f = fixtures();
    let cases = f["matchers"].as_array().unwrap();
    assert!(cases.len() > 300);
    for (i, case) in cases.iter().enumerate() {
        let kind = case["kind"].as_str().unwrap();
        let expected_blocks = blocks(&case["blocks"]);
        let expected_ops = chunks(&case["opcodes"]);
        let (got_blocks, got_ops) = match kind {
            "inline" => {
                let a: Vec<char> = case["a"].as_str().unwrap().chars().collect();
                let b: Vec<char> = case["b"].as_str().unwrap().chars().collect();
                let mut m = MyersSequenceMatcher::inline(&a, &b);
                (m.matching_blocks().to_vec(), m.opcodes())
            }
            "line" => {
                let (a, b) = (strings(&case["a"]), strings(&case["b"]));
                let mut m = MyersSequenceMatcher::new(&a, &b);
                (m.matching_blocks().to_vec(), m.opcodes())
            }
            "sync" => {
                let (a, b) = (strings(&case["a"]), strings(&case["b"]));
                let points = case["syncpoints"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| (usize_of(&p[0]), usize_of(&p[1])))
                    .collect();
                let mut m = MyersSequenceMatcher::with_syncpoints(&a, &b, points);
                (m.matching_blocks().to_vec(), m.opcodes())
            }
            other => panic!("unknown kind {other}"),
        };
        assert_eq!(got_blocks, expected_blocks, "case {i} ({kind}) blocks");
        assert_eq!(got_ops, expected_ops, "case {i} ({kind}) opcodes");
    }
}

#[test]
fn unified_diff_matches_python() {
    let f = fixtures();
    for (i, case) in f["unified"].as_array().unwrap().iter().enumerate() {
        let (a, b) = (strings(&case["a"]), strings(&case["b"]));
        let opts = UnifiedDiffOptions {
            fromfile: "from/a.txt",
            tofile: "to/b.txt",
            n: usize_of(&case["n"]),
            ..Default::default()
        };
        assert_eq!(
            unified_diff(&a, &b, &opts),
            case["diff"].as_str().unwrap(),
            "case {i}"
        );
    }
}

#[test]
fn patches_match_python() {
    let f = fixtures();
    for (i, case) in f["patches"].as_array().unwrap().iter().enumerate() {
        let texts = strings(&case["texts"]);
        let labels = strings(&case["labels"]);
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let side = if case["left"].as_bool().unwrap() {
            PatchSide::Left
        } else {
            PatchSide::Right
        };
        let patch = make_patch(&texts, &labels, side, case["reverse"].as_bool().unwrap()).unwrap();
        assert_eq!(patch, case["patch"].as_str().unwrap(), "case {i}");
    }
}

fn check_snapshot(differ: &Differ, changes: &DiffsChanged, snap: &Value, what: &str) {
    assert_eq!(
        differ.all_changes(),
        merged_list(&snap["merge_cache"]),
        "{what}: merge cache"
    );
    for (pane, cache) in snap["line_cache"].as_array().unwrap().iter().enumerate() {
        let expected: Vec<ChunkLocation> = cache
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|run| {
                let loc = ChunkLocation {
                    current: opt_usize(&run[1]),
                    prev: opt_usize(&run[2]),
                    next: opt_usize(&run[3]),
                };
                std::iter::repeat_n(loc, usize_of(&run[0]))
            })
            .collect();
        let got: Vec<ChunkLocation> = (0..expected.len())
            .map(|l| differ.locate_chunk(pane, l))
            .collect();
        assert_eq!(got, expected, "{what}: line cache of pane {pane}");
        assert_eq!(
            differ.locate_chunk(pane, expected.len()),
            ChunkLocation::default(),
            "{what}: line cache length of pane {pane}"
        );
    }
    let conflicts: Vec<usize> = snap["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .map(usize_of)
        .collect();
    assert_eq!(differ.conflicts(), conflicts, "{what}: conflicts");
    for (pane, m) in snap["mergeable"].as_array().unwrap().iter().enumerate() {
        let expected = (m[0].as_bool().unwrap(), m[1].as_bool().unwrap());
        assert_eq!(
            differ.has_mergeable_changes(pane),
            expected,
            "{what}: mergeable {pane}"
        );
    }
    assert_eq!(
        sorted_by_json(changes.removed.clone()),
        sorted_by_json(clamped_list(&snap["removed"])),
        "{what}: removed"
    );
    assert_eq!(
        sorted_by_json(changes.added.clone()),
        sorted_by_json(merged_list(&snap["added"])),
        "{what}: added"
    );
    let modified = (!snap["modified"].is_null()).then(|| {
        [
            clamped_chunk(&snap["modified"][0]),
            clamped_chunk(&snap["modified"][1]),
        ]
    });
    assert_eq!(changes.modified, modified, "{what}: modified");
    assert_eq!(
        differ.sequences_identical(),
        snap["identical"].as_bool().unwrap(),
        "{what}: identical"
    );
    let unresolved: Vec<usize> = snap["unresolved"]
        .as_array()
        .unwrap()
        .iter()
        .map(usize_of)
        .collect();
    assert_eq!(differ.unresolved(), unresolved, "{what}: unresolved");
}

fn lines_of(text: &str) -> Vec<String> {
    // Merged texts are "\n".join(lines); recover the lines Python joined.
    text.split('\n').map(str::to_owned).collect()
}

#[test]
fn differ_matches_python() {
    let f = fixtures();
    let cases = f["differ"].as_array().unwrap();
    assert!(cases.len() >= 100);
    for (i, case) in cases.iter().enumerate() {
        let texts: Vec<Vec<String>> = case["texts"]
            .as_array()
            .unwrap()
            .iter()
            .map(strings)
            .collect();
        let mut differ = Differ::new();
        differ.auto_merge = case["auto_merge"].as_bool().unwrap();
        differ.ignore_blanks = case["ignore_blanks"].as_bool().unwrap();
        let changes = differ.set_sequences(&texts).unwrap();

        if texts.len() == 3 {
            let marked = merge_3_files(&differ, &texts, true);
            let expected = &case["merge3_marked"];
            assert_eq!(
                marked.text(),
                expected["text"].as_str().unwrap(),
                "case {i}: merge3 marked"
            );
            if !marked.lines.is_empty() {
                assert_eq!(marked.lines, lines_of(expected["text"].as_str().unwrap()));
            }
            let unresolved: Vec<usize> = expected["unresolved"]
                .as_array()
                .unwrap()
                .iter()
                .map(usize_of)
                .collect();
            assert_eq!(marked.unresolved, unresolved, "case {i}: merge3 unresolved");
            let unmarked = merge_3_files(&differ, &texts, false);
            assert_eq!(
                unmarked.text(),
                case["merge3_unmarked"]["text"].as_str().unwrap(),
                "case {i}: merge3 unmarked"
            );
            assert!(unmarked.unresolved.is_empty());
            if differ.auto_merge {
                differ.set_unresolved(marked.unresolved);
            }
        }
        check_snapshot(
            &differ,
            &changes,
            &case["initial"],
            &format!("case {i} initial"),
        );

        for m in case["merge2"].as_array().unwrap() {
            let (from, to) = (usize_of(&m[0]), usize_of(&m[1]));
            let got = merge_2_files(&differ, &texts, from, to);
            assert_eq!(
                got.text(),
                m[2].as_str().unwrap(),
                "case {i}: merge2 {from}->{to}"
            );
        }

        let mut current = texts.clone();
        for (j, edit) in case["edits"].as_array().unwrap().iter().enumerate() {
            let replace = &edit["replace"];
            let (lo, hi) = (usize_of(&replace[0]), usize_of(&replace[1]));
            current[usize_of(&edit["pane"])].splice(lo..hi, strings(&replace[2]));
            let size = edit["size"].as_i64().unwrap() as isize;
            let changes = differ
                .change_sequence(
                    usize_of(&edit["pane"]),
                    usize_of(&edit["start"]),
                    size,
                    &current,
                )
                .unwrap();
            check_snapshot(
                &differ,
                &changes,
                &edit["after"],
                &format!("case {i} edit {j}"),
            );
        }

        let q = &case["queries"];
        for g in q["get_chunk"].as_array().unwrap() {
            let (index, from) = (usize_of(&g[0]), usize_of(&g[1]));
            let to = opt_usize(&g[2]);
            assert_eq!(
                differ.get_chunk(index, from, to),
                chunk(&g[3]),
                "case {i}: get_chunk({index}, {from}, {to:?})"
            );
        }
        for entry in q["chunk_starts"].as_array().unwrap() {
            let index = usize_of(&entry[0]);
            let expected: Vec<Option<usize>> =
                entry[1].as_array().unwrap().iter().map(opt_usize).collect();
            assert_eq!(
                differ.get_chunk_starts(index).to_vec(),
                expected,
                "case {i}: starts {index}"
            );
        }
        for p in q["pair_changes"].as_array().unwrap() {
            let (from, to) = (usize_of(&p[0]), usize_of(&p[1]));
            let lines = p[2].as_array().map(|l| {
                [
                    usize_of(&l[0]),
                    usize_of(&l[1]),
                    usize_of(&l[2]),
                    usize_of(&l[3]),
                ]
            });
            assert_eq!(
                differ.pair_changes(from, to, lines),
                chunks(&p[3]),
                "case {i}: pair_changes({from}, {to}, {lines:?})"
            );
        }
        for s in q["single_changes"].as_array().unwrap() {
            let pane = usize_of(&s[0]);
            let lines = s[1].as_array().map(|l| (usize_of(&l[0]), usize_of(&l[1])));
            assert_eq!(
                differ.single_changes(pane, lines),
                chunks(&s[2]),
                "case {i}: single_changes({pane}, {lines:?})"
            );
        }
    }
}
