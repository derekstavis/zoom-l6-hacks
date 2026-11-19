#!/usr/bin/env python3
"""Check L6max bootloaderless handoff before MAIN runs and after reset.

Usage: boot-handoff-qtest.py QEMU L6MAX_MAIN PATCHED_L6MAX_MAIN L6_MAIN
Each MAIN is an unpacked firmware component; no device state is modified.
"""
import json
import select
import socket
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


def check_flexram_boundaries(qemu, image):
    qmp_host, qmp_guest = socket.socketpair()
    qmp_host.settimeout(10)
    process = subprocess.Popen(
        [qemu, "-M", "l6max-main", "-kernel", image, "-accel", "qtest",
         "-qtest", "stdio", "-display", "none",
         "-chardev", f"socket,id=handoff-qmp,fd={qmp_guest.fileno()}",
         "-mon", "chardev=handoff-qmp,mode=control"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        text=True, bufsize=1, pass_fds=(qmp_guest.fileno(),),
    )
    qmp_guest.close()
    qmp = qmp_host.makefile("rwb", buffering=0)

    def command(text):
        process.stdin.write(text + "\n")
        process.stdin.flush()
        if not select.select([process.stdout], [], [], 10)[0]:
            raise RuntimeError(f"no response to {text}")
        reply = process.stdout.readline().strip()
        if not reply.startswith("OK"):
            raise RuntimeError(f"{text}: {reply}")
        return reply

    try:
        assert "QMP" in json.loads(qmp.readline())
        def qmp_execute(name):
            qmp.write(json.dumps({"execute": name}).encode() + b"\n")
            while True:
                reply = json.loads(qmp.readline())
                if "error" in reply:
                    raise AssertionError(reply["error"])
                if "return" in reply:
                    return
        qmp_execute("qmp_capabilities")
        for last, beyond in ((0x7ffc, 0x8000), (0x2001fffc, 0x20020000),
                             (0x202d7ffc, 0x202d8000)):
            command(f"writel {last:#x} 0x1234abcd")
            command(f"writel {beyond:#x} 0x5678efab")
            assert int(command(f"readl {last:#x}").split()[1], 16) == 0x1234abcd
            assert int(command(f"readl {beyond:#x}").split()[1], 16) != 0x5678efab
        print(f"{image}: FlexRAM mapping boundaries passed")
        masks = {
            0x400ac004: 0xfff9f000, 0x400ac008: 0xfb00ffff,
            0x400ac014: 0xcfffffff, 0x400ac068: 0xefeffffb,
            0x400ac06c: 0xedffffff, 0x400ac070: 0xfffffff6,
        }
        for address in masks:
            command(f"writel {address:#x} 0xffffffff")
        for address in (0x400fc070, 0x400fc078):
            command(f"writel {address:#x} 0")
        qmp_execute("system_reset")
        for address, expected in masks.items():
            assert int(command(f"readl {address:#x}").split()[1], 16) == expected
        assert int(command("readl 0x400fc070").split()[1], 16) == 0x0c000030
        assert int(command("readl 0x400fc078").split()[1], 16) == 0xc
        print(f"{image}: masked GPR and gate reset passed")
    finally:
        qmp.close()
        qmp_host.close()
        process.terminate()
        process.wait(timeout=5)


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
        0x400ac040: 0x00200007, 0x400ac044: 0x5555ea95,
        0x400fc068: 0xc0000000, 0x400fc06c: 0xcc0c0000,
        0x400fc070: 0x0c000030, 0x400fc074: 0x30, 0x400fc078: 0xc,
        0x400fc080: 0xc00,
        0xe000e010: 0, 0xe000ed08: 0x80000000,
        0xe000ed14: 0x200, 0xe000ed24: 0, 0xe000ed94: 0,
    }
    check(qemu, stock, l6max)
    check(qemu, patched, l6max)
    check_flexram_boundaries(qemu, stock)
    check(qemu, l6, {0x401f8204: 0, 0x402f0010: 0})


if __name__ == "__main__":
    main()
