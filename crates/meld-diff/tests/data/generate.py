#!/usr/bin/env python3
"""Generate differential-test fixtures for meld-diff from the Python Meld code.

It imports the Python Meld modules, which are no longer in the tree: run it
from a checkout of commit 29d59e07 (the last one with them), from the
repository root:

    PYTHONPATH=. python3 crates/meld-diff/tests/data/generate.py

Writes python_fixtures.json next to this script. Inputs are random but
seeded, so the output is reproducible.
"""

import difflib
import json
import os
import random

from meld.matchers import myers
from meld.matchers.diffutil import Differ
from meld.matchers.merge import AutoMergeDiffer, Merger

rng = random.Random(20261022)


def chunk(c):
    if c is None:
        return None
    return [c[0], c[1], c[2], c[3], c[4]]


def blocks(bs):
    return [[b[0], b[1], b[2]] for b in bs]


def rand_lines(n, alphabet):
    return [rng.choice(alphabet) for _ in range(n)]


def mutate(lines, alphabet, rate=0.3):
    out = []
    for line in lines:
        r = rng.random()
        if r < rate / 3:
            continue
        elif r < 2 * rate / 3:
            out.append(rng.choice(alphabet))
        elif r < rate:
            out.append(line)
            out.extend(rand_lines(rng.randint(1, 3), alphabet))
        else:
            out.append(line)
    if rng.random() < 0.3:
        out[0:0] = rand_lines(rng.randint(1, 4), alphabet)
    return out


def alphabet(size, blank=False):
    alpha = [f"line {i}" for i in range(size)]
    if blank:
        alpha += ["", ""]
    return alpha


# --- Matchers ---------------------------------------------------------------

def matcher_cases():
    cases = []
    for i in range(150):
        size = rng.choice([2, 4, 8, 30, 200])
        alpha = alphabet(size)
        n = rng.choice([0, 1, 5, 20, 60, 150])
        a = rand_lines(n, alpha)
        b = mutate(a, alpha, rate=rng.choice([0.1, 0.3, 0.7]))
        if rng.random() < 0.2:
            b = rand_lines(rng.randint(0, 80), alpha)
        m = myers.MyersSequenceMatcher(None, a, b)
        cases.append({
            "kind": "line",
            "a": a,
            "b": b,
            "blocks": blocks(m.get_matching_blocks()),
            "opcodes": [chunk(c) for c in m.get_opcodes()],
        })
    for i in range(150):
        chars = rng.choice(["ab", "abc ", "abcdefgh ,.", "abcdefghijklmnopqrstuvwxyz é中"])
        n = rng.choice([0, 2, 3, 10, 40, 120])
        a = "".join(rng.choice(chars) for _ in range(n))
        b = "".join(mutate(list(a), list(chars), rate=rng.choice([0.1, 0.4]))) if a else ""
        if rng.random() < 0.2:
            b = "".join(rng.choice(chars) for _ in range(rng.randint(0, 60)))
        m = myers.InlineMyersSequenceMatcher(None, a, b)
        cases.append({
            "kind": "inline",
            "a": a,
            "b": b,
            "blocks": blocks(m.get_matching_blocks()),
            "opcodes": [chunk(c) for c in m.get_opcodes()],
        })
    for i in range(80):
        alpha = alphabet(rng.choice([3, 10, 40]))
        a = rand_lines(rng.randint(1, 60), alpha)
        b = mutate(a, alpha)
        k = rng.randint(1, 3)
        pa = sorted(rng.randint(0, len(a)) for _ in range(k))
        pb = sorted(rng.randint(0, len(b)) for _ in range(k))
        syncpoints = list(zip(pa, pb))
        m = myers.SyncPointMyersSequenceMatcher(None, a, b, syncpoints)
        cases.append({
            "kind": "sync",
            "a": a,
            "b": b,
            "syncpoints": [list(p) for p in syncpoints],
            "blocks": blocks(m.get_matching_blocks()),
            "opcodes": [chunk(c) for c in m.get_opcodes()],
        })
    return cases


# --- difflib.unified_diff / patches ------------------------------------------

def unified_cases():
    cases = []
    for i in range(80):
        alpha = [s + "\n" for s in alphabet(rng.choice([3, 10, 50]))]
        if rng.random() < 0.3:
            alpha += ["crlf\r\n", "no newline"]
        n = rng.choice([0, 1, 5, 30, 120, 260])
        a = rand_lines(n, alpha)
        b = mutate(a, alpha, rate=rng.choice([0.02, 0.1, 0.4]))
        ctx = rng.choice([0, 1, 3, 3, 5])
        out = "".join(difflib.unified_diff(a, b, "from/a.txt", "to/b.txt", n=ctx))
        cases.append({"a": a, "b": b, "n": ctx, "diff": out})
    return cases


def update_patch(texts, labels, left_patch, reverse_patch):
    """The logic of PatchDialog.update_patch, without GTK."""
    indices = (0, 1)
    if not left_patch:
        indices = (1, 2)
    if reverse_patch:
        indices = (indices[1], indices[0])

    split = []
    for text in texts:
        lines = text.splitlines(True)
        barelines = text.splitlines(False)
        if barelines and lines and barelines[-1] == lines[-1]:
            if len(lines) > 1:
                previous_linebreak = lines[-2][len(barelines[-2]):]
            else:
                previous_linebreak = "\n"
            lines[-1] += previous_linebreak
        split.append(lines)

    names = [labels[i] for i in range(3)]
    prefix = os.path.commonprefix(names)
    names = [n[prefix.rfind("/") + 1:] for n in names]
    text0, text1 = split[indices[0]], split[indices[1]]
    name0, name1 = names[indices[0]], names[indices[1]]
    return "".join(difflib.unified_diff(text0, text1, name0, name1))


def patch_cases():
    cases = []
    label_sets = [
        ["/home/u/p/a.txt", "/home/u/p/b.txt", "/home/u/p/c.txt"],
        ["/home/u/p/a.txt", "/home/u/q/b.txt", "<unnamed>"],
        ["/x/file.txt", "/x/file.txt.orig", "/x/file.txt.new"],
        ["a", "b", "c"],
    ]
    for i in range(40):
        seps = rng.choice([["\n"], ["\r\n"], ["\n", "\r\n", "\r"], ["\n", " ", "\x0c"]])
        alpha = alphabet(rng.choice([3, 8]))
        texts = []
        base = rand_lines(rng.randint(0, 20), alpha)
        for _ in range(3):
            lines = mutate(base, alpha) if base else rand_lines(rng.randint(0, 3), alpha)
            text = "".join(line + rng.choice(seps) for line in lines)
            if text and rng.random() < 0.5:
                text = text[:-1] if not text.endswith("\r\n") else text[:-2]
            texts.append(text)
        labels = rng.choice(label_sets)
        left = rng.random() < 0.5
        reverse = rng.random() < 0.5
        cases.append({
            "texts": texts,
            "labels": labels,
            "left": left,
            "reverse": reverse,
            "patch": update_patch(texts, labels, left, reverse),
        })
    return cases


# --- Differ -------------------------------------------------------------------

def rle(cache):
    """Run-length encode a line cache as [count, current, prev, next] runs."""
    runs = []
    for entry in cache:
        if runs and runs[-1][1:] == list(entry):
            runs[-1][0] += 1
        else:
            runs.append([1, *entry])
    return runs


def snapshot(differ, changes):
    removed, added, modified = changes
    norm = lambda m: [chunk(m[0]), chunk(m[1])]
    order = lambda m: (json.dumps(norm(m)))
    return {
        "merge_cache": [norm(m) for m in differ._merge_cache],
        "line_cache": [rle(cache) for cache in differ._line_cache],
        "conflicts": list(differ.conflicts),
        "mergeable": [list(differ.has_mergeable_changes(i)) for i in range(3)],
        "removed": sorted((norm(m) for m in removed), key=json.dumps),
        "added": sorted((norm(m) for m in added), key=json.dumps),
        "modified": norm(modified) if modified else None,
        "identical": differ.sequences_identical(),
        "unresolved": list(getattr(differ, "unresolved", [])),
    }


def run_set_sequences(differ, texts):
    captured = []
    handler = differ.connect("diffs-changed", lambda d, c: captured.append(c))
    for _ in differ.set_sequences_iter(texts):
        pass
    differ.disconnect(handler)
    return captured[-1]


def run_change(differ, pane, start, size, texts):
    captured = []
    handler = differ.connect("diffs-changed", lambda d, c: captured.append(c))
    differ.change_sequence(pane, start, size, texts)
    differ.disconnect(handler)
    return captured[-1]


def random_edit(texts, pane, alpha):
    """Edit a pane like a buffer edit would; returns (start, size, (lo, hi, new))."""
    lines = texts[pane]
    if not lines:
        return None
    s = rng.randrange(len(lines))
    kind = rng.random()
    if kind < 0.35:
        new, k, hi = [rng.choice(alpha)], 0, s + 1
    elif kind < 0.7:
        k = rng.randint(1, 4)
        new, hi = rand_lines(k + 1, alpha), s + 1
    else:
        k = rng.randint(1, min(4, len(lines) - s - 1)) if s < len(lines) - 1 else 0
        new, hi = [rng.choice(alpha + [lines[s]])], s + k + 1
        k = -k
    lines[s:hi] = new
    return (s, k, (s, hi, new))


def queries(differ, texts):
    """Range and per-chunk queries on the final state."""
    n = differ.num_sequences
    out = {"get_chunk": [], "pair_changes": [], "single_changes": [], "chunk_starts": []}
    count = differ.diff_count()
    for i in sorted(rng.sample(range(count), min(count, 4))):
        for from_pane in range(n):
            for to_pane in [None] + list(range(n)):
                c = differ.get_chunk(i, from_pane, to_pane)
                out["get_chunk"].append([i, from_pane, to_pane, chunk(c)])
        out["chunk_starts"].append([i, differ.get_chunk_starts(i)])
    for q in range(3):
        lens = [len(t) for t in texts]
        frm, to = rng.choice([(1, 0), (0, 1), (1, 2), (2, 1)] if n == 3 else [(1, 0), (0, 1)])
        lo0 = rng.randint(0, lens[frm])
        hi0 = rng.randint(lo0, lens[frm])
        lo1 = rng.randint(0, lens[to])
        hi1 = rng.randint(lo1, lens[to])
        res = [chunk(c) for c in differ.pair_changes(frm, to, (lo0, hi0, lo1, hi1))]
        out["pair_changes"].append([frm, to, [lo0, hi0, lo1, hi1], res])
        if q == 0:
            res = [chunk(c) for c in differ.pair_changes(frm, to)]
            out["pair_changes"].append([frm, to, None, res])
        pane = rng.randrange(n)
        lo = rng.randint(0, lens[pane])
        hi = rng.randint(lo, lens[pane])
        res = [chunk(c) for c in differ.single_changes(pane, (lo, hi))]
        out["single_changes"].append([pane, [lo, hi], res])
        if q == 0:
            res = [chunk(c) for c in differ.single_changes(pane)]
            out["single_changes"].append([pane, None, res])
    return out


def merge_text(differ, texts, mode, args=()):
    merger = Merger()
    merger.differ = differ
    merger.texts = texts
    gen = merger.merge_3_files(*args) if mode == 3 else merger.merge_2_files(*args)
    result = None
    for result in gen:
        pass
    return result, list(merger.unresolved)


def differ_cases():
    cases = []
    for i in range(120):
        panes = rng.choice([2, 3, 3])
        blank = rng.random() < 0.4
        alpha = alphabet(rng.choice([3, 6, 15, 60]), blank=blank)
        base = rand_lines(rng.choice([0, 1, 4, 12, 30, 80]), alpha)
        if not base:
            base = [rng.choice(alpha)]
        texts = [mutate(base, alpha, rng.choice([0.1, 0.3, 0.6])) or [""] for _ in range(panes)]
        texts[1] = base[:] if rng.random() < 0.5 else texts[1]
        auto = panes == 3 and rng.random() < 0.5
        differ = AutoMergeDiffer() if auto else Differ()
        if auto:
            differ.auto_merge = True
        differ.ignore_blanks = blank and rng.random() < 0.7
        initial = [t[:] for t in texts]
        case = {
            "texts": initial,
            "auto_merge": auto,
            "ignore_blanks": differ.ignore_blanks,
        }
        case["initial"] = snapshot(differ, run_set_sequences(differ, texts))

        if panes == 3:
            merged, unresolved = merge_text(differ, texts, 3, (True,))
            case["merge3_marked"] = {"text": merged, "unresolved": unresolved}
            merged, unresolved = merge_text(differ, texts, 3, (False,))
            case["merge3_unmarked"] = {"text": merged, "unresolved": unresolved}
            if auto:
                differ.unresolved = case["merge3_marked"]["unresolved"][:]
                case["initial"]["unresolved"] = differ.unresolved[:]
        merge2 = []
        for frm, to in ([(0, 1), (1, 0)] if panes == 2 else [(0, 1), (2, 1), (1, 0), (1, 2)]):
            merged, _ = merge_text(differ, texts, 2, (frm, to))
            merge2.append([frm, to, merged])
        case["merge2"] = merge2

        edits = []
        for _ in range(rng.choice([0, 3, 8])):
            pane = rng.randrange(panes)
            edit = random_edit(texts, pane, alpha)
            if edit is None:
                continue
            start, size, (lo, hi, new) = edit
            changes = run_change(differ, pane, start, size, texts)
            edits.append({
                "pane": pane,
                "start": start,
                "size": size,
                "replace": [lo, hi, new],
                "after": snapshot(differ, changes),
            })
        case["edits"] = edits
        case["queries"] = queries(differ, texts)
        cases.append(case)
    return cases


def main():
    fixtures = {
        "matchers": matcher_cases(),
        "unified": unified_cases(),
        "patches": patch_cases(),
        "differ": differ_cases(),
    }
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "python_fixtures.json")
    with open(path, "w", encoding="utf-8") as f:
        json.dump(fixtures, f, ensure_ascii=False, separators=(",", ":"))
    print(path, os.path.getsize(path))


if __name__ == "__main__":
    main()
