#!/usr/bin/env python3
"""
Analyze glide JSONL trace data.

Usage:
    # Compare labeled runs
    python3 scripts/analyze.py intentional.jsonl accidental.jsonl

    # Single file
    python3 scripts/analyze.py recording.jsonl

    # Pipe from glide
    sudo glide --record --label intentional | python3 scripts/analyze.py -
"""

import argparse
import json
import sys
from collections import defaultdict


def load_records(path):
    """Load JSONL records from a file or stdin."""
    source = sys.stdin if path == "-" else open(path)
    records = []
    for line in source:
        line = line.strip()
        if not line:
            continue
        try:
            records.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    if source is not sys.stdin:
        source.close()
    return records


def extract_episodes(records):
    """Extract episode_summary records."""
    return [r for r in records if r.get("type") == "episode_summary"]


def extract_samples(records):
    """Extract sample records."""
    return [r for r in records if r.get("type") == "sample"]


def print_episode_table(episodes, title="Episodes"):
    """Print a formatted table of episode summaries."""
    if not episodes:
        print(f"\n{title}: (none)")
        return

    print(f"\n{'=' * 90}")
    print(f"  {title} ({len(episodes)} episodes)")
    print(f"{'=' * 90}")
    print(
        f"  {'ID':>4}  {'Label':<12} {'Dur(ms)':>8} {'Samples':>8} "
        f"{'Motion%':>8} {'MeanDisp':>9} {'MaxDisp':>8} "
        f"{'LongRun':>8} {'Activ':>6} {'ActivMs':>8} {'KB':>4}"
    )
    print(f"  {'-' * 86}")

    for ep in episodes:
        print(
            f"  {ep.get('episode_id', '?'):>4}  "
            f"{(ep.get('label') or ''):.<12} "
            f"{ep['duration_ms']:>8.0f} "
            f"{ep['total_samples']:>8} "
            f"{ep['motion_ratio'] * 100:>7.1f}% "
            f"{ep['mean_displacement']:>9.1f} "
            f"{ep['max_displacement']:>8.1f} "
            f"{ep['longest_motion_run']:>8} "
            f"{'YES' if ep['activated'] else 'no':>6} "
            f"{ep.get('activation_latency_ms', 0) or 0:>8.0f} "
            f"{ep.get('kb_presses_during', 0):>4}"
        )


def print_distribution(episodes, field, title):
    """Print basic distribution stats for a numeric field."""
    values = [ep[field] for ep in episodes if field in ep]
    if not values:
        return
    values.sort()
    n = len(values)
    mean = sum(values) / n
    median = values[n // 2]
    p10 = values[max(0, int(n * 0.1))]
    p90 = values[min(n - 1, int(n * 0.9))]

    print(f"  {title}: n={n} min={values[0]:.1f} p10={p10:.1f} "
          f"median={median:.1f} mean={mean:.1f} p90={p90:.1f} max={values[-1]:.1f}")


def analyze_separation(intentional, accidental):
    """Analyze how well features separate intentional from accidental episodes."""
    if not intentional or not accidental:
        return

    print(f"\n{'=' * 90}")
    print(f"  SEPARATION ANALYSIS")
    print(f"{'=' * 90}")

    fields = [
        ("duration_ms", "Duration (ms)"),
        ("total_samples", "Total samples"),
        ("motion_ratio", "Motion ratio"),
        ("mean_displacement", "Mean displacement"),
        ("max_displacement", "Max displacement"),
        ("total_displacement", "Total displacement"),
        ("longest_motion_run", "Longest motion run"),
    ]

    for field, title in fields:
        int_vals = sorted([ep[field] for ep in intentional if field in ep])
        acc_vals = sorted([ep[field] for ep in accidental if field in ep])

        if not int_vals or not acc_vals:
            continue

        int_min, int_max = int_vals[0], int_vals[-1]
        acc_min, acc_max = acc_vals[0], acc_vals[-1]
        int_med = int_vals[len(int_vals) // 2]
        acc_med = acc_vals[len(acc_vals) // 2]

        # Check if there's a clean threshold that separates the classes
        overlap = max(0, min(int_max, acc_max) - max(int_min, acc_min))
        total_range = max(int_max, acc_max) - min(int_min, acc_min)
        overlap_pct = (overlap / total_range * 100) if total_range > 0 else 0

        sep = "CLEAN" if overlap == 0 else f"{overlap_pct:.0f}% overlap"

        print(f"\n  {title}:")
        print(f"    Intentional: min={int_min:.1f} median={int_med:.1f} max={int_max:.1f}")
        print(f"    Accidental:  min={acc_min:.1f} median={acc_med:.1f} max={acc_max:.1f}")
        print(f"    Separation:  {sep}")

        # Suggest threshold if separation exists
        if overlap == 0 and int_med > acc_med:
            threshold = (acc_max + int_min) / 2
            print(f"    → Suggested threshold: > {threshold:.1f} (all intentional above, all accidental below)")
        elif overlap == 0 and acc_med > int_med:
            threshold = (int_max + acc_min) / 2
            print(f"    → Suggested threshold: < {threshold:.1f}")


def main():
    parser = argparse.ArgumentParser(description="Analyze glide JSONL traces")
    parser.add_argument("files", nargs="+", help="JSONL trace files (use - for stdin)")
    parser.add_argument("--activated-only", action="store_true", help="Only show activated episodes")
    parser.add_argument("--min-duration", type=float, default=0, help="Min episode duration (ms)")
    parser.add_argument("--csv", action="store_true", help="Output episode summaries as CSV")
    args = parser.parse_args()

    all_episodes = []
    by_label = defaultdict(list)

    for path in args.files:
        records = load_records(path)
        episodes = extract_episodes(records)

        for ep in episodes:
            if args.activated_only and not ep.get("activated"):
                continue
            if ep["duration_ms"] < args.min_duration:
                continue
            all_episodes.append(ep)
            label = ep.get("label") or "unlabeled"
            by_label[label].append(ep)

    if args.csv:
        import csv
        if all_episodes:
            w = csv.DictWriter(sys.stdout, fieldnames=all_episodes[0].keys())
            w.writeheader()
            for ep in all_episodes:
                w.writerow(ep)
        return

    # Print per-label tables
    for label, episodes in sorted(by_label.items()):
        print_episode_table(episodes, f"{label.upper()} episodes")

        print(f"\n  Distributions:")
        print_distribution(episodes, "duration_ms", "Duration")
        print_distribution(episodes, "motion_ratio", "Motion ratio")
        print_distribution(episodes, "mean_displacement", "Mean displacement")
        print_distribution(episodes, "longest_motion_run", "Longest motion run")

    # Separation analysis if we have both intentional and accidental
    intentional = by_label.get("intentional", [])
    accidental = by_label.get("accidental", [])
    if intentional and accidental:
        analyze_separation(intentional, accidental)

    # Overall summary
    print(f"\n{'=' * 90}")
    print(f"  SUMMARY")
    print(f"{'=' * 90}")
    for label, episodes in sorted(by_label.items()):
        n_activated = sum(1 for ep in episodes if ep.get("activated"))
        print(f"  {label}: {len(episodes)} episodes, {n_activated} activated ({n_activated * 100 // max(len(episodes), 1)}%)")


if __name__ == "__main__":
    main()
