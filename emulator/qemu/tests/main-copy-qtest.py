#!/usr/bin/env python3
"""Exercise MAIN copy bounds and reset using synthetic, disposable NOR images.

Usage: main-copy-qtest.py /path/to/qemu-system-arm
No physical-device capacity or update acceptance is inferred by this test.
"""
import json
import os
from pathlib import Path
import select
import socket
import struct
import subprocess
import sys
import tempfile

SLOT = 0x1A2FF8
NOR_MAIN = 0x50000
BASE = 0x80000000


def run(qemu, directory, model):
    image = bytearray(b"\xff" * (SLOT if model == "l6max" else 0x2000))
    reset = 0x80000415 if model == "l6max" else 0x80001415
    struct.pack_into("<II", image, 0, 0x20230000, reset)
    if model == "l6max":
        struct.pack_into("<I", image, SLOT - 4, 0x12345678)
    kernel = directory / "main_firmware.bin"
    kernel.write_bytes(image)
    (directory / "main_trailer.bin").write_bytes(struct.pack("<I4s", len(image), b"0110"))
    (directory / "secondary_firmware.bin").write_bytes(b"\xff")
    (directory / "secondary_trailer.bin").write_bytes(struct.pack("<I4s", 1, b"0100"))
    (directory / "resource_region.bin").write_bytes(b"\xff")
    state = directory / "state"
    state.mkdir()
    nor = bytearray(b"\xff" * 0x200000)
    nor[NOR_MAIN:NOR_MAIN + len(image)] = image
    struct.pack_into("<I4s", nor, 0x1f2ff8, len(image), b"0110")
    struct.pack_into("<I4s", nor, 0x1f7ff8, len(image), b"0110")
    nor_path = state / "main-nor.bin"
    nor_path.write_bytes(nor)

    host, guest = socket.socketpair()
    host.settimeout(10)
    env = dict(os.environ, L6_STATE_DIR=str(state))
    env.pop("L6_EXTERNAL_RAM_FD", None)
    process = subprocess.Popen(
        [qemu, "-M", "l6max-main", "-kernel", str(kernel), "-accel", "qtest",
         "-qtest", "stdio", "-display", "none",
         "-chardev", f"socket,id=copy-qmp,fd={guest.fileno()}",
         "-mon", "chardev=copy-qmp,mode=control"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        text=True, bufsize=1, pass_fds=(guest.fileno(),), env=env,
    )
    guest.close()
    qmp = host.makefile("rwb", buffering=0)

    def command(text):
        process.stdin.write(text + "\n")
        process.stdin.flush()
        if not select.select([process.stdout], [], [], 10)[0]:
            raise RuntimeError(f"no response to {text}")
        reply = process.stdout.readline().split()
        assert reply and reply[0] == "OK", (text, reply)
        return int(reply[1], 16) if len(reply) > 1 else None

    def qmp_execute(name):
        qmp.write(json.dumps({"execute": name}).encode() + b"\n")
        while True:
            reply = json.loads(qmp.readline())
            if "error" in reply:
                raise AssertionError(reply["error"])
            if "return" in reply:
                return

    def write_nor(offset, value):
        # This is an isolated test file. MAP_SHARED exposes the changed NOR to
        # QEMU while qtest holds the CPU stopped; no device state is reused.
        with nor_path.open("r+b", buffering=0) as f:
            f.seek(offset)
            f.write(struct.pack("<I", value))

    try:
        assert "QMP" in json.loads(qmp.readline())
        qmp_execute("qmp_capabilities")
        if model == "l6max":
            # A maximum-length image includes its final four bytes, while the
            # following four bytes belong outside the package MAIN slot.
            assert command(f"readl {BASE + SLOT - 4:#x}") == 0x12345678
            command(f"writel {BASE + 0x100:#x} 0x76543210")
            assert command(f"readl {BASE + 0x100:#x}") == 0x76543210
            write_nor(NOR_MAIN + 0x100, 0x01234567)
            write_nor(NOR_MAIN + SLOT - 4, 0x10203040)
            qmp_execute("system_reset")
            assert command(f"readl {BASE + 0x100:#x}") == 0x01234567
            assert command(f"readl {BASE + SLOT - 4:#x}") == 0x10203040

            # The installed footer takes priority; bytes outside its copy
            # length retain SDRAM contents on this warm reset.
            write_nor(0x1f7ff8, 0x1000)
            command(f"writel {BASE + 0x1000:#x} 0xa1b2c3d4")
            write_nor(NOR_MAIN + 0x100, 0x55667788)
            qmp_execute("system_reset")
            assert command(f"readl {BASE + 0x100:#x}") == 0x55667788
            assert command(f"readl {BASE + 0x1000:#x}") == 0xa1b2c3d4

            # Older state without a valid footer uses the full slot, even if
            # the package trailer contains a valid but stale shorter length.
            write_nor(0x1f7ff8, 0xffffffff)
            write_nor(0x1f2ff8, 0x1100)
            write_nor(NOR_MAIN + 0x10fc, 0x11223344)
            write_nor(NOR_MAIN + 0x1100, 0x31415926)
            command(f"writel {BASE + 0x1100:#x} 0xabcdef01")
            qmp_execute("system_reset")
            assert command(f"readl {BASE + 0x10fc:#x}") == 0x11223344
            assert command(f"readl {BASE + 0x1100:#x}") == 0x31415926
            assert command(f"readl {BASE + SLOT - 4:#x}") == 0x10203040

            # Both lengths invalid: compatibility fallback is bounded to the
            # MAIN slot, excluding the fixed trailer immediately following it.
            write_nor(0x1f2ff8, 0xffffffff)
            command(f"writel {BASE + SLOT:#x} 0xabcdef01")
            qmp_execute("system_reset")
            assert command(f"readl {BASE + SLOT - 4:#x}") == 0x10203040
            assert command(f"readl {BASE + SLOT:#x}") == 0xabcdef01
        else:
            # L6 retains its existing read-only image mapping and reset path.
            assert command("readl 0x80001004") == reset
            command("writel 0x80001004 0x11223344")
            assert command("readl 0x80001004") == reset
            qmp_execute("system_reset")
            assert command("readl 0x80001004") == reset
        print(f"{model}: MAIN copy/reset bounds passed")
    finally:
        qmp.close()
        host.close()
        process.terminate()
        process.wait(timeout=10)


with tempfile.TemporaryDirectory(prefix="l6-main-copy-") as temporary:
    for model in ("l6max", "l6"):
        directory = Path(temporary) / model
        directory.mkdir()
        run(sys.argv[1], directory, model)

    # Refuse an image that extends even one byte into the fixed trailer.
    oversized = Path(temporary) / "oversized"
    oversized.mkdir()
    image = bytearray(b"\xff" * (SLOT + 1))
    struct.pack_into("<II", image, 0, 0x20230000, 0x80000415)
    kernel = oversized / "main_firmware.bin"
    kernel.write_bytes(image)
    env = dict(os.environ)
    env.pop("L6_STATE_DIR", None)
    env.pop("L6_EXTERNAL_RAM_FD", None)
    result = subprocess.run(
        [sys.argv[1], "-M", "l6max-main", "-kernel", str(kernel),
         "-accel", "qtest", "-display", "none"],
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True, timeout=10, env=env,
    )
    assert result.returncode != 0 and "cannot load NOR segment" in result.stderr
    print("l6max: oversized MAIN rejected")
