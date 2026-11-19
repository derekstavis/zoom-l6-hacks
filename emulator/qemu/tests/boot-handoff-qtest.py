#!/usr/bin/env python3
"""Check L6max bootloaderless handoff before MAIN runs and after reset.

Usage: boot-handoff-qtest.py QEMU L6MAX_MAIN PATCHED_L6MAX_MAIN L6_MAIN
Each MAIN is an unpacked firmware component; no device state is modified.
"""
import json
import subprocess
import sys


def check(qemu, image, expected):
    reads = [
        {"execute": "human-monitor-command",
         "arguments": {"command-line": f"xp /1wx {address:#x}"}}
        for address in expected
    ]
    commands = ([{"execute": "qmp_capabilities"}] + reads +
                [{"execute": "system_reset"}] + reads + [{"execute": "quit"}])
    result = subprocess.run(
        [qemu, "-M", "l6max-main", "-kernel", image,
         "-display", "none", "-S", "-qmp", "stdio"],
        input="\n".join(map(json.dumps, commands)) + "\n",
        capture_output=True, text=True, timeout=30, check=True,
    )
    values = []
    for line in result.stdout.splitlines():
        reply = json.loads(line)
        if "error" in reply:
            raise AssertionError(reply["error"])
        value = reply.get("return")
        if isinstance(value, str) and ": 0x" in value:
            values.append(int(value.strip().split(": 0x")[1], 16))
    assert values == list(expected.values()) * 2, (image, values)
    print(f"{image}: initial handoff and reset passed")


def main():
    if len(sys.argv) != 5:
        raise SystemExit(__doc__)
    qemu, stock, patched, l6 = sys.argv[1:]
    l6max = {
        0x401b8000: 4, 0x401b8004: 0x10000004,
        0x401c0000: 8, 0x401c0004: 9,
        0x401f8204: 0x10f0, 0x402f0010: 0x8000001b,
        0x402f0014: 0x8200001b, 0x402f0040: 0xf31,
        0x402f0044: 0x00664b22, 0x402f0048: 0x0009090b,
        0x402f004c: 0x08070a01, 0x400a8000: 5,
        0x400fc068: 0xc0000000, 0x400fc06c: 0xcc0c0000,
        0x400fc070: 0x0c000000, 0x400fc074: 0x30,
        0x400fc080: 0xc00,
        0xe000ed14: 0x200, 0xe000ed94: 0,
    }
    check(qemu, stock, l6max)
    check(qemu, patched, l6max)
    check(qemu, l6, {0x401f8204: 0, 0x402f0010: 0})


if __name__ == "__main__":
    main()
