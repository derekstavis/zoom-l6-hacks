#!/usr/bin/env python3
"""Check MAIN input CLOCK heartbeat against QEMU virtual time without firmware.

Usage: main-clock-qtest.py /path/to/qemu-system-arm
"""
import os
from pathlib import Path
import select
import socket
import struct
import subprocess
import sys
import tempfile

MAGIC = 0x4c364950
CLOCK = 0x80000008


def run(qemu, model):
    with tempfile.TemporaryDirectory(prefix='l6-main-clock-') as temp:
        root = Path(temp)
        firmware = root / 'main_firmware.bin'
        image = bytearray(b'\xff' * (0x1a2ff8 if model == 'l6max' else 8192))
        struct.pack_into('<II', image, 0, 0x20230000,
                         0x80000415 if model == 'l6max' else 0x80001415)
        firmware.write_bytes(image)
        (root / 'main_trailer.bin').write_bytes(struct.pack('<I4s', len(image), b'0110'))
        (root / 'secondary_firmware.bin').write_bytes(b'\xff')
        (root / 'secondary_trailer.bin').write_bytes(struct.pack('<I4s', 1, b'0100'))
        (root / 'resource_region.bin').write_bytes(b'\xff')
        footer = bytearray(b'\xff' * 64)
        struct.pack_into('<I4s', footer, 0x38, len(image), b'0110')
        (root / 'package_footer.bin').write_bytes(footer)
        state = root / 'state'
        state.mkdir()
        host, guest = socket.socketpair()
        host.settimeout(2)
        env = dict(os.environ, L6_STATE_DIR=str(state), L6_INPUT_FD=str(guest.fileno()))
        env.pop('L6_EXTERNAL_RAM_FD', None)
        proc = subprocess.Popen(
            [qemu, '-M', 'l6max-main', '-kernel', str(firmware), '-accel', 'qtest',
             '-qtest', 'stdio', '-display', 'none'], stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
            bufsize=1, pass_fds=(guest.fileno(),), env=env)
        guest.close()
        try:
            def step(ns):
                proc.stdin.write(f'clock_step {ns}\n')
                proc.stdin.flush()
                if not select.select([proc.stdout], [], [], 5)[0]:
                    raise RuntimeError('qtest clock_step timed out')
                reply = proc.stdout.readline().split()
                assert reply[0] == 'OK', reply
            # Advance in 1 ms increments to exercise the recurring input timer.
            for _ in range(99):
                step(1_000_000)
            assert not select.select([host], [], [], 0.05)[0], 'early heartbeat'
            step(1_000_000)
            raw = b''
            while len(raw) < 24:
                raw += host.recv(24 - len(raw))
            magic, kind, seq, target, value, millis = struct.unpack('<IIIIiI', raw)
            assert (magic, kind, seq, target, value, millis) == (MAGIC, CLOCK, 1, 0, 0, 100)
            for _ in range(100):
                step(1_000_000)
            raw = b''
            while len(raw) < 24:
                raw += host.recv(24 - len(raw))
            assert struct.unpack('<IIIIiI', raw) == (MAGIC, CLOCK, 2, 0, 0, 200)
            print(f'{model}: MAIN heartbeat follows virtual clock')
        finally:
            proc.terminate()
            proc.wait(timeout=5)
            host.close()


if __name__ == '__main__':
    for selected_model in ('l6', 'l6max'):
        run(sys.argv[1], selected_model)
