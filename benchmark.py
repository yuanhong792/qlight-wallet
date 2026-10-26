#!/usr/bin/env python3
"""ML-DSA transfer throughput calculator + optional local benchmark.

Examples:
    python3 benchmark.py
    python3 benchmark.py --scheme ml-dsa-44 --block-size 4MB --block-time 2 --utilization 0.9
    python3 benchmark.py --bench --iters 500

The TPS model is intentionally simple:
    tx_size = header + signature + (first_use ? public_key : 0)
    tx_per_block = floor(block_size * utilization / tx_size)
    tps = tx_per_block / block_time

It is an upper bound for simple transfers, not a full node benchmark.
"""

import argparse
import math
import platform
import statistics
import time

# FIPS 204 sizes (bytes)
SCHEMES = {
    "ml-dsa-44": {"pk": 1312, "sk": 2560, "sig": 2420, "level": 2},
    "ml-dsa-65": {"pk": 1952, "sk": 4032, "sig": 3309, "level": 3},
    "ml-dsa-87": {"pk": 2592, "sk": 4896, "sig": 4627, "level": 5},
    "ecdsa": {"pk": 33, "sk": 32, "sig": 64, "level": 0},
}

DEFAULT_HEADER = 128          # account-model header + one recipient + nonce + fee
DEFAULT_BLOCK_SIZES = [
    ("256KB", 256 * 1024),
    ("512KB", 512 * 1024),
    ("1MB", 1024 * 1024),
    ("2MB", 2 * 1024 * 1024),
    ("4MB", 4 * 1024 * 1024),
    ("8MB", 8 * 1024 * 1024),
    ("16MB", 16 * 1024 * 1024),
]
DEFAULT_BLOCK_TIMES = [1, 2, 6, 12]


def parse_size(value):
    """Parse '1MB', '512KB', '1048576' into bytes."""
    text = str(value).strip().upper().replace(" ", "")
    units = {"KB": 1024, "MB": 1024 ** 2, "GB": 1024 ** 3, "B": 1}
    for suffix, multiplier in units.items():
        if text.endswith(suffix):
            return int(float(text[: -len(suffix)]) * multiplier)
    return int(text)


def tx_size(header, sig_size, first_use_pk=0):
    return header + sig_size + first_use_pk


def tps(block_bytes, block_time, size, utilization=1.0):
    if size <= 0 or block_time <= 0:
        raise ValueError("size and block_time must be positive")
    per_block = math.floor(block_bytes * utilization / size)
    return per_block / block_time


def fmt(value, digits=1):
    return f"{value:,.{digits}f}"


def print_matrix(sig_size, header, block_sizes, block_times, utilization, first_use_pk=0):
    size = tx_size(header, sig_size, first_use_pk)
    print()
    print(f"Transaction size: {size:,} bytes "
          f"(header={header}, sig={sig_size}, first-use-pk={first_use_pk})")
    print(f"Utilization: {utilization:.0%}")
    print()
    header_cells = " | ".join(f"{t}s" for t in block_times)
    print(f"| Block size \\ Block time | {header_cells} |")
    print("|---:" + "|---:" * len(block_times) + "|")
    for label, block_bytes in block_sizes:
        cells = " | ".join(fmt(tps(block_bytes, t, size, utilization)) for t in block_times)
        print(f"| {label} | {cells} |")


def print_bandwidth(sig_size, header, first_use_pk=0):
    size = tx_size(header, sig_size, first_use_pk)
    print()
    print("| TPS | MB/s | Mbps |")
    print("|---:|---:|---:|")
    for rate in (100, 500, 1000, 2000, 5000, 10000):
        bytes_per_sec = rate * size
        print(f"| {rate:,} | {bytes_per_sec / 1e6:,.2f} | {bytes_per_sec * 8 / 1e6:,.1f} |")


def print_batch(sig_size, header, block_bytes, block_time):
    print()
    print("| Transfers per tx | Tx size (bytes) | Amortized bytes/transfer | Transfers/s |")
    print("|---:|---:|---:|---:|")
    for n in (1, 10, 100, 1000):
        total = header + (n - 1) * 32 + sig_size
        per_block = math.floor(block_bytes / total)
        rate = per_block * n / block_time
        print(f"| {n:,} | {total:,} | {total / n:,.1f} | {rate:,.0f} |")


def run_benchmark(iters, message_size):
    try:
        from pqcrypto.sign import ml_dsa_44, ml_dsa_65, ml_dsa_87
    except ImportError:
        print("pqcrypto is not installed. Run:")
        print("  python3 -m venv .venv && .venv/bin/pip install pqcrypto")
        return

    modules = [
        ("ML-DSA-44", ml_dsa_44),
        ("ML-DSA-65", ml_dsa_65),
        ("ML-DSA-87", ml_dsa_87),
    ]
    message = b"x" * message_size
    print(f"Machine: {platform.platform()} / {platform.machine()}")
    print(f"Python: {platform.python_version()}  iterations: {iters}")
    print()
    print("| Scheme | pk (B) | sk (B) | sig (B) | keygen ms (p50/p95) | sign ms (p50/p95) | verify ms (p50/p95) |")
    print("|---|---:|---:|---:|---:|---:|---:|")

    for name, mod in modules:
        for _ in range(5):
            mod.keygen()
        pk, sk = mod.keygen()

        for _ in range(20):
            mod.sign(sk, message)
        sample_sig = mod.sign(sk, message)
        for _ in range(5):
            mod.verify(pk, message, sample_sig)

        def bench(fn, count):
            samples = []
            for _ in range(count):
                start = time.perf_counter_ns()
                fn()
                samples.append((time.perf_counter_ns() - start) / 1e6)
            return statistics.median(samples), statistics.quantiles(samples, n=100)[94]

        keygen = bench(mod.keygen, max(30, iters // 5))
        sign = bench(lambda: mod.sign(sk, message), iters)
        verify = bench(lambda: mod.verify(pk, message, sample_sig), iters)
        print(f"| {name} | {len(pk):,} | {len(sk):,} | {len(sample_sig):,} | "
              f"{keygen[0]:.3f} / {keygen[1]:.3f} | {sign[0]:.3f} / {sign[1]:.3f} | "
              f"{verify[0]:.3f} / {verify[1]:.3f} |")


def main():
    parser = argparse.ArgumentParser(description="ML-DSA transfer TPS calculator")
    parser.add_argument("--scheme", default="ml-dsa-65", choices=sorted(SCHEMES),
                        help="signature scheme preset (default: ml-dsa-65)")
    parser.add_argument("--sig-size", type=int, default=None, help="override signature size in bytes")
    parser.add_argument("--header", type=int, default=DEFAULT_HEADER, help="tx header+payload bytes")
    parser.add_argument("--first-use-pk", type=int, default=0,
                        help="add public key bytes for first-use transactions")
    parser.add_argument("--block-size", default=None, help="single block size, e.g. 4MB")
    parser.add_argument("--block-time", type=float, default=None, help="single block time in seconds")
    parser.add_argument("--utilization", type=float, default=1.0, help="0..1, default 1.0 (upper bound)")
    parser.add_argument("--batch", action="store_true", help="print batch-transfer amortization table")
    parser.add_argument("--bench", action="store_true", help="run local ML-DSA benchmark (needs pqcrypto)")
    parser.add_argument("--iters", type=int, default=300, help="benchmark iterations")
    parser.add_argument("--message-size", type=int, default=128, help="benchmark message bytes")
    args = parser.parse_args()

    if not 0 < args.utilization <= 1:
        parser.error("--utilization must be in (0, 1]")

    sig_size = args.sig_size if args.sig_size is not None else SCHEMES[args.scheme]["sig"]
    print(f"Scheme: {args.scheme} (sig={sig_size:,} B, header={args.header:,} B)")

    if args.block_size and args.block_time:
        block_bytes = parse_size(args.block_size)
        value = tps(block_bytes, args.block_time, tx_size(args.header, sig_size, args.first_use_pk),
                    args.utilization)
        print(f"Block: {block_bytes:,} B / {args.block_time}s, "
              f"tx: {tx_size(args.header, sig_size, args.first_use_pk):,} B")
        print(f"TPS upper bound: {value:,.1f}")
        print(f"Bandwidth: {value * tx_size(args.header, sig_size, args.first_use_pk) / 1e6:,.2f} MB/s")
        return

    print_matrix(sig_size, args.header, DEFAULT_BLOCK_SIZES, DEFAULT_BLOCK_TIMES, args.utilization,
                 args.first_use_pk)
    print_bandwidth(sig_size, args.header, args.first_use_pk)
    if args.batch:
        print_batch(sig_size, args.header, 1024 * 1024, 2)
    if args.bench:
        print()
        run_benchmark(args.iters, args.message_size)


if __name__ == "__main__":
    main()
