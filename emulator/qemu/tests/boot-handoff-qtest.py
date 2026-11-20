#!/usr/bin/env python3
"""Check L6max bootloaderless handoff before MAIN runs and after reset.

Usage: boot-handoff-qtest.py QEMU L6MAX_MAIN PATCHED_L6MAX_MAIN L6_MAIN
Each MAIN is an unpacked firmware component; no device state is modified.
"""
import json
import os
import select
import socket
import subprocess
import sys


def isolated_env():
    env = dict(os.environ)
    env.pop("L6_STATE_DIR", None)
    env.pop("L6_EXTERNAL_RAM_FD", None)
    return env


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
        capture_output=True, text=True, timeout=30, check=True, env=isolated_env(),
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
        text=True, bufsize=1, pass_fds=(qmp_guest.fileno(),), env=isolated_env(),
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
        def qmp_execute(name, arguments=None):
            request = {"execute": name}
            if arguments is not None:
                request["arguments"] = arguments
            qmp.write(json.dumps(request).encode() + b"\n")
            while True:
                reply = json.loads(qmp.readline())
                if "error" in reply:
                    raise AssertionError(reply["error"])
                if "return" in reply:
                    return reply["return"]
        qmp_execute("qmp_capabilities")
        for last, beyond in ((0x7ffc, 0x8000), (0x2001fffc, 0x20020000),
                             (0x202d7ffc, 0x202d8000)):
            command(f"writel {last:#x} 0x1234abcd")
            command(f"writel {beyond:#x} 0x5678efab")
            assert int(command(f"readl {last:#x}").split()[1], 16) == 0x1234abcd
            assert int(command(f"readl {beyond:#x}").split()[1], 16) != 0x5678efab
        print(f"{image}: FlexRAM mapping boundaries passed")
        main_vector = int(command("readl 0x80000000").split()[1], 16)
        command("writel 0x80000000 0x1234abcd")
        assert int(command("readl 0x80000000").split()[1], 16) == 0x1234abcd
        # Program four bytes in the emulated NOR through FlexSPI, leaving the
        # current SDRAM image alone until the next bootloaderless reset.
        main_word = int(command("readl 0x80000100").split()[1], 16)
        assert main_word != 0
        command("writel 0x402a8200 0x06")       # write enable
        command("writel 0x402a80a4 0")
        command("writel 0x402a80b0 1")
        command("writel 0x402a8200 0x02")       # page program
        command("writel 0x402a80a0 0x50100")
        command("writel 0x402a80a4 4")
        command("writel 0x402a80b0 1")
        command("writel 0x402a8180 0")
        command("writel 0x402a8014 0x40")       # commit TX FIFO
        assert int(command("readl 0x80000100").split()[1], 16) == main_word
        command("writel 0x402a8200 0x06")
        command("writel 0x402a80a4 0")
        command("writel 0x402a80b0 1")
        command("writel 0x402a8200 0x02")
        command("writel 0x402a80a0 0x50000")
        command("writel 0x402a80a4 4")
        command("writel 0x402a80b0 1")
        command("writel 0x402a8180 0x20200000")
        command("writel 0x402a8014 0x40")
        masks = {
            0x400ac004: 0xfff9f000, 0x400ac008: 0xfb00ffff,
            0x400ac014: 0xcfffffff, 0x400ac068: 0xefeffffb,
            0x400ac06c: 0xedffffff, 0x400ac070: 0xfffffff6,
        }
        for address in masks:
            command(f"writel {address:#x} 0xffffffff")
        for address in (0x400fc070, 0x400fc078):
            command(f"writel {address:#x} 0")
        command("writel 0x400fc06c 0x00007000")
        clocks = {
            0x400fc000: (0xffffff00, 0x7f),
            0x400fc00c: (0xfffffffe, 0),
            0x400fc020: (0xfffffc03, 0x00180204),
            0x400fc024: (0xfff8c780, 0x06010800),
            0x400fc02c: (0xfffffe00, 0xc1),
            0x400fc030: (0xf1ff8fff, 0x03f01f80),
            0x400fc038: (0xfe03ffff, 0),
            0x400fc060: (0xfe00fe00, 0x00120001),
        }
        for address in clocks:
            command(f"writel {address:#x} 0xffffffff")
        # PLL_AUDIO aliases update one control register and LOCK follows
        # ENABLE/POWERDOWN instead of accepting writes to the status bit.
        command("writel 0x400d8078 0x2000")
        assert not int(command("readl 0x400d8070").split()[1], 16) & 0x80000000
        command("writel 0x400d8074 0x2000")
        assert int(command("readl 0x400d8074").split()[1], 16) & 0x80000000
        command("writel 0x400d8074 0x1000")
        assert not int(command("readl 0x400d8070").split()[1], 16) & 0x80000000
        command("writel 0x400d8078 0x1000")
        assert int(command("readl 0x400d8078").split()[1], 16) & 0x80000000
        command("writel 0x400d807c 0x10000")
        assert int(command("readl 0x400d8070").split()[1], 16) & 0x10000
        command("writel 0x400d8070 0x80000000")
        assert int(command("readl 0x400d8070").split()[1], 16) == 0
        for base in (0x400d80f0, 0x400d8100):
            original = int(command(f"readl {base:#x}").split()[1], 16)
            command(f"writel {base + 4:#x} 0x80")
            assert int(command(f"readl {base + 4:#x}").split()[1], 16) == original | 0x80
            command(f"writel {base + 8:#x} 0x80")
            assert int(command(f"readl {base:#x}").split()[1], 16) == original & ~0x80
            command(f"writel {base + 12:#x} 0x80")
            assert int(command(f"readl {base:#x}").split()[1], 16) == original ^ 0x80
            command(f"writel {base:#x} 0xffffffff")
            assert int(command(f"readl {base:#x}").split()[1], 16) == (
                (original & 0x40404040) | 0xbfbfbfbf
            )
        for address in (0x400d8020, 0x400d80a0, 0x400d80e0, 0x400d8160, 0x400d8170):
            command(f"writel {address:#x} 0xffffffff")
        command("writel 0x400d8070 0x00080000")
        # Exercise handoff cleanup from a dirty CPU, rather than merely
        # comparing QEMU's reset defaults on an untouched guest.
        for address, value in ((0xe000e100, 1), (0xe000e200, 1),
                               (0xe000e010, 7), (0xe000ed94, 5),
                               (0xe000ed24, 0x70000)):
            command(f"writel {address:#x} {value:#x}")
            assert int(command(f"readl {address:#x}").split()[1], 16) & value == value
        for region in (0, 6, 15):
            command(f"writel 0xe000ed98 {region}")
            command("writel 0xe000ed9c 0x20200000")
            command("writel 0xe000eda0 0x03000019")
            assert int(command("readl 0xe000eda0").split()[1], 16) == 0x03000019
        qmp_execute("system_reset")
        assert int(command("readl 0x80000000").split()[1], 16) == main_vector & 0x20200000
        assert int(command("readl 0x80000100").split()[1], 16) == 0
        registers = qmp_execute("human-monitor-command", {"command-line": "info registers"})
        assert "R13=20200000" in registers, registers
        for address, expected in masks.items():
            assert int(command(f"readl {address:#x}").split()[1], 16) == expected
        assert int(command("readl 0x400fc070").split()[1], 16) == 0x0c000030
        assert int(command("readl 0x400fc078").split()[1], 16) == 0xc
        assert int(command("readl 0x400fc06c").split()[1], 16) == 0xcc0c4000
        for address, (keep, value) in clocks.items():
            assert int(command(f"readl {address:#x}").split()[1], 16) == keep | value
        for address, value in ((0x400d8020, 0x12000), (0x400d80a0, 0x13000),
                               (0x400d80e0, 0x80312001),
                               (0x400d8070, 0x80082020),
                               (0x400d8160, 0xffffffe0),
                               (0x400d8170, 0x3f7f7fff)):
            assert int(command(f"readl {address:#x}").split()[1], 16) == value
        for address in (0xe000e100, 0xe000e200, 0xe000e010,
                        0xe000ed94, 0xe000ed24):
            assert int(command(f"readl {address:#x}").split()[1], 16) == 0
        assert int(command("readl 0xe000ed08").split()[1], 16) == 0x80000000
        assert not int(command("readl 0xe000ed14").split()[1], 16) & 0x30000
        assert int(command("readl 0xe000ed90").split()[1], 16) == 0x1000
        for region in range(16):
            command(f"writel 0xe000ed98 {region}")
            rbar = int(command("readl 0xe000ed9c").split()[1], 16)
            rasr = int(command("readl 0xe000eda0").split()[1], 16)
            assert (rbar & ~0x1f, rasr) == (0, 0), (region, hex(rbar), hex(rasr))
        # The clock initializer disables a broad set of gates, then later
        # setup enables the known GPIO, memory-controller and IOMUX gates.
        gate_results = {
            0x400fc068: 0xc0000fff, 0x400fc06c: 0xfc0ccc00,
            0x400fc070: 0xfc3f303f, 0x400fc074: 0xffffff30,
            0x400fc07c: 0xf0033ff3, 0x400fc080: 0xffff3fc3,
            0x400fc084: 0xfffffc33,
        }
        # A valid installed footer limits the next copy; bytes beyond it
        # retain SDRAM contents even when NOR holds different bytes.
        command("writel 0x402a8200 0x06")
        command("writel 0x402a80a4 0")
        command("writel 0x402a80b0 1")
        command("writel 0x402a8200 0x02")
        command("writel 0x402a80a0 0x1f7ff8")
        command("writel 0x402a80a4 4")
        command("writel 0x402a80b0 1")
        command("writel 0x402a8180 0x100")
        command("writel 0x402a8014 0x40")
        command("writel 0x80000100 0xdeadbeef")
        for address in gate_results:
            command(f"writel {address:#x} 0xffffffff")
        qmp_execute("system_reset")
        assert int(command("readl 0x80000100").split()[1], 16) == 0xdeadbeef
        for address, expected in gate_results.items():
            assert int(command(f"readl {address:#x}").split()[1], 16) == expected
        print(f"{image}: masked GPR, gate and CPU reset passed")
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
        0x00000000: 0,
        0x401b8000: 4, 0x401b8004: 0x10000004,
        0x401c0000: 8, 0x401c0004: 9,
        0x401f8204: 0x10f0, 0x402f0010: 0x8000001b,
        0x402f0014: 0x8200001b, 0x402f0040: 0xf31,
        0x402f0044: 0x00664b22, 0x402f0048: 0x0009090b,
        0x402f004c: 0x08070a01, 0x400a8000: 5,
        0x400ac040: 0x00200007, 0x400ac044: 0x5555ea95,
        0x400fc068: 0xc0000fff, 0x400fc06c: 0xfc0ccc00,
        0x400fc070: 0xfc3f303f, 0x400fc074: 0xffffff30,
        0x400fc078: 0xffffffff, 0x400fc07c: 0xf0033ff3,
        0x400fc080: 0xffff3fc3, 0x400fc084: 0xfffffc33,
        0x400fc000: 0x0401107f, 0x400fc00c: 0x100,
        0x400fc010: 1, 0x400fc014: 0x00098340, 0x400fc018: 0x7dae8324,
        0x400fc020: 0x13192e06, 0x400fc024: 0x06490b00,
        0x400fc02c: 0x007336c1, 0x400fc030: 0x33f71f92,
        0x400fc038: 0x00029150, 0x400fc060: 0x00120001,
        0x400d8020: 0x12000, 0x400d8070: 0x80002020,
        0x400d8170: 0x00272727,
        0x400d80a0: 0x13000, 0x400d80e0: 0x80312001,
        0xe000e010: 0, 0xe000ed08: 0x80000000,
        0xe000ed14: 0x200, 0xe000ed24: 0, 0xe000ed94: 0,
    }
    check(qemu, stock, l6max)
    check(qemu, patched, l6max)
    check_flexram_boundaries(qemu, stock)
    check(qemu, l6, {0x401f8204: 0, 0x402f0010: 0})


if __name__ == "__main__":
    main()
