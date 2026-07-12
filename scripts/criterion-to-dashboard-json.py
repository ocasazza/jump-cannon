#!/usr/bin/env python3
"""Convert criterion estimates.json tree into the dashboard JSON files.

Reads target/criterion/*/.../new/estimates.json and produces three files:
  benches_wide_tp.json       — timeseries: one row per run, columns per benchmark
  benches_wide_base<N>.json  — baseline snapshot for regression comparison
  latest_base<N>.json        — latest run vs baseline delta

Usage:
  python3 scripts/criterion-to-dashboard-json.py \\
    --criterion-dir target/criterion \\
    --out-dir $out/benchdata \\
    --timestamp $(date -u +%Y-%m-%dT%H:%M:%SZ) \\
    [--baseline-run N]
"""

import argparse
import json
import os
import sys
from pathlib import Path


def load_estimates(criterion_dir: Path) -> dict[str, dict]:
    """Walk criterion dir, load every new/estimates.json, return {bench_id: estimates}."""
    results = {}
    for estimates_path in sorted(criterion_dir.glob("**/new/estimates.json")):
        # Path is like: pagerank/gpu/10000/new/estimates.json
        # Benchmark ID is the path relative to criterion_dir minus /new/estimates.json
        rel = estimates_path.relative_to(criterion_dir)
        bench_id = str(rel.parent.parent).replace("/", "_")  # pagerank_gpu_10000
        with open(estimates_path) as f:
            results[bench_id] = json.load(f)
    return results


def throughput_melem_s(estimates: dict) -> float:
    """Compute throughput in millions of elements per second."""
    mean_ns = estimates["mean"]["estimate"]  # nanoseconds
    # throughput is a list; take the first element's per_iteration
    tp = estimates.get("throughput", [])
    if not tp:
        return 0.0
    elements = tp[0]["per_iteration"]
    seconds = mean_ns / 1e9
    if seconds == 0:
        return 0.0
    return (elements / seconds) / 1e6


def build_wide_row(results: dict[str, dict], timestamp: str) -> dict:
    """Build one row for benches_wide_tp.json."""
    row = {"timestamp": timestamp}
    for bench_id, est in sorted(results.items()):
        row[bench_id] = round(throughput_melem_s(est), 4)
    return row


def build_latest_row(results: dict[str, dict], baseline: dict[str, dict] | None) -> list[dict]:
    """Build rows for latest_base<N>.json — one per benchmark with delta."""
    rows = []
    for bench_id, est in sorted(results.items()):
        tp = throughput_melem_s(est)
        row = {
            "full_id": bench_id,
            "throughput_melem_s": round(tp, 4),
        }
        if baseline and bench_id in baseline:
            base_tp = throughput_melem_s(baseline[bench_id])
            if base_tp > 0:
                row["delta_pct"] = round(((tp - base_tp) / base_tp) * 100, 2)
            else:
                row["delta_pct"] = 0.0
        else:
            row["delta_pct"] = 0.0
        rows.append(row)
    return rows


def main():
    parser = argparse.ArgumentParser(description="Convert criterion output to dashboard JSON")
    parser.add_argument("--criterion-dir", required=True, type=Path, help="Path to target/criterion")
    parser.add_argument("--out-dir", required=True, type=Path, help="Output directory for JSON files")
    parser.add_argument("--timestamp", required=True, help="ISO 8601 timestamp for this run")
    parser.add_argument("--baseline-run", type=int, default=1, help="Which run number is the baseline")
    args = parser.parse_args()

    results = load_estimates(args.criterion_dir)
    if not results:
        print("No criterion estimates found — skipping dashboard JSON generation", file=sys.stderr)
        return

    args.out_dir.mkdir(parents=True, exist_ok=True)

    # --- benches_wide_tp.json (timeseries) ---
    # Append to existing file so each Hydra run adds a row
    wide_path = args.out_dir / "benches_wide_tp.json"
    rows = []
    if wide_path.exists():
        with open(wide_path) as f:
            try:
                rows = json.load(f)
            except json.JSONDecodeError:
                rows = []
    rows.append(build_wide_row(results, args.timestamp))
    with open(wide_path, "w") as f:
        json.dump(rows, f, indent=2)

    # --- benches_wide_base<N>.json (baseline snapshot) ---
    # Save the first N runs as baselines; overwrite if this run number matches
    base_path = args.out_dir / f"benches_wide_base{args.baseline_run}.json"
    with open(base_path, "w") as f:
        json.dump([build_wide_row(results, args.timestamp)], f, indent=2)

    # --- latest_base<N>.json (latest vs baseline delta) ---
    # Load the baseline snapshot to compute deltas
    baseline_results = None
    if base_path.exists():
        with open(base_path) as f:
            base_rows = json.load(f)
            if base_rows:
                # Reconstruct a {bench_id: {mean: {estimate: ...}}} dict from the wide row
                # We need the original estimates for the baseline, not just throughput.
                # For now, store the baseline wide row and compute delta from throughput.
                pass

    # For latest, we need the baseline estimates, not just throughput.
    # Store baseline estimates separately.
    baseline_est_path = args.out_dir / f"baseline{args.baseline_run}_estimates.json"
    if not baseline_est_path.exists():
        # Serialize just the mean.estimate for each benchmark
        baseline_est = {
            bid: {"mean": {"estimate": est["mean"]["estimate"]},
                  "throughput": est.get("throughput", [])}
            for bid, est in results.items()
        }
        with open(baseline_est_path, "w") as f:
            json.dump(baseline_est, f, indent=2)

    # Load baseline estimates for delta computation
    baseline_est = {}
    if baseline_est_path.exists():
        with open(baseline_est_path) as f:
            baseline_est = json.load(f)

    latest_path = args.out_dir / f"latest_base{args.baseline_run}.json"
    latest_rows = build_latest_row(results, baseline_est)
    with open(latest_path, "w") as f:
        json.dump(latest_rows, f, indent=2)

    print(f"Wrote {len(rows)} rows to {wide_path}", file=sys.stderr)
    print(f"Wrote baseline to {base_path}", file=sys.stderr)
    print(f"Wrote latest to {latest_path}", file=sys.stderr)


if __name__ == "__main__":
    main()