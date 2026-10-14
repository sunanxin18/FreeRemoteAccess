#!/usr/bin/env python3
# SPDX-License-Identifier: LGPL-2.1-or-later
"""独立 Python big-int oracle；stdin 必须为保存的完整 clock 测试输出。"""
import sys

sentinel = 0x13579BDF2468ACE0
maximum = (1 << 64) - 1
signed_maximum = (1 << 63) - 1
expected_rows = 10036


def expected_inputs():
    """独立声明 finite tuples 和 uint64 seed 规则，不复制 helper 算术。"""
    for frequency in (1, 3, 10_000_000, signed_maximum):
        for delta in (0, 1, frequency - 1, frequency, frequency + 1, maximum):
            yield delta, frequency
    yield from (
        (1, 0), (1, signed_maximum + 1),
        (18_446_744_073, 1), (18_446_744_074, 1),
        (55_340_232_221, 3), (55_340_232_222, 3),
        (184_467_440_737_095_516, 10_000_000),
        (184_467_440_737_095_517, 10_000_000),
        (184_467_440_737, 10), (184_467_440_738, 10),
        (maximum, 1_000_000_000), (maximum, 999_999_999),
    )
    seed = 0x87A4FC1D529B03E7

    def advance(state):
        # C uint64 shift/XOR 的输入生成语义；各左移后显式模 2^64。
        state = (state ^ (state << 13)) & maximum
        state ^= state >> 7
        return (state ^ (state << 17)) & maximum

    for _ in range(10000):
        seed = advance(seed)
        delta = seed
        seed = advance(seed)
        frequency = seed & signed_maximum
        yield delta, frequency or 1


rows = 0
for expected_delta, expected_frequency in expected_inputs():
    parts = sys.stdin.readline().split()
    if len(parts) != 5 or parts[0] != "CLOCK":
        raise SystemExit(f"FAIL unexpected/truncated clock output row={rows}")
    try:
        delta, frequency, status, value = map(int, parts[1:])
    except ValueError:
        raise SystemExit(f"FAIL non-integer clock field row={rows}") from None
    if (delta, frequency) != (expected_delta, expected_frequency):
        raise SystemExit(f"FAIL clock tuple row={rows}")
    valid_frequency = 1 <= expected_frequency <= signed_maximum
    # 直接大整数乘除，与 C whole/rem/binary helper 不共享算法。
    expected = expected_delta * 1_000_000_000 // expected_frequency if valid_frequency else maximum + 1
    success = valid_frequency and expected <= maximum
    if status != (0 if success else -1) or value != (expected if success else sentinel):
        raise SystemExit(f"FAIL oracle row={rows} delta={delta} frequency={frequency}")
    rows += 1
if rows != expected_rows or sys.stdin.readline().split() != ["CLOCK-END", str(expected_rows)]:
    raise SystemExit("FAIL missing/wrong clock end marker or declared row count")
if sys.stdin.readline() != "":
    raise SystemExit("FAIL extra clock output after end marker")
print(f"PASS independent big-int oracle rows={rows}")
