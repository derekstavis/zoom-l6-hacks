#!/usr/bin/env python3
"""Check L6max bootloaderless handoff before MAIN runs and after reset.

Usage: boot-handoff-qtest.py QEMU L6MAX_MAIN PATCHED_L6MAX_MAIN L6_MAIN
Each MAIN is an unpacked firmware component; no device state is modified.
"""
import json
import os
import select
import socket
import struct
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


def check_main_stack_entry(qemu, image, invalid=0x8000, error_bit=0x08, mpu_test=False):
    # Bootloader 1.00 calls the reset handler with its current task stack.
    # MAIN 1.10's first six instructions install its own vector-table MSP
    # without accessing that inherited stack. Run those instructions with a
    # deliberately different MSP to verify this revision-specific property.
    host, guest = socket.socketpair()
    qmp_host, qmp_guest = socket.socketpair()
    host.settimeout(10)
    qmp_host.settimeout(10)
    process = subprocess.Popen(
        [qemu, "-M", "l6max-main", "-kernel", image, "-S", "-display", "none",
         "-chardev", f"socket,id=entry-gdb,fd={guest.fileno()}",
         "-gdb", "chardev:entry-gdb",
         "-chardev", f"socket,id=entry-qmp,fd={qmp_guest.fileno()}",
         "-mon", "chardev=entry-qmp,mode=control"],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL, pass_fds=(guest.fileno(), qmp_guest.fileno()),
        env=isolated_env(),
    )
    guest.close()
    qmp_guest.close()
    qmp = qmp_host.makefile("rwb", buffering=0)

    def packet(request):
        body = request.encode("ascii")
        host.sendall(b"$" + body + b"#" + f"{sum(body) & 255:02x}".encode())
        while True:
            char = host.recv(1)
            if not char:
                raise EOFError("GDB connection closed")
            if char == b"$":
                break
        response = bytearray()
        while True:
            char = host.recv(1)
            if char == b"#":
                break
            if not char:
                raise EOFError("GDB connection closed")
            response.extend(char)
        checksum = bytearray()
        while len(checksum) < 2:
            chunk = host.recv(2 - len(checksum))
            if not chunk:
                raise EOFError("GDB connection closed")
            checksum.extend(chunk)
        assert int(checksum, 16) == sum(response) & 255
        host.sendall(b"+")
        return response.decode("ascii")

    def registers():
        data = bytes.fromhex(packet("g"))
        return [int.from_bytes(data[i:i + 4], "little")
                for i in range(0, 16 * 4, 4)]

    try:
        # With no persisted NOR, the package trailer supplies the copy size.
        trailer_path = os.path.join(os.path.dirname(image), "main_trailer.bin")
        try:
            with open(trailer_path, "rb") as trailer:
                copy_size = int.from_bytes(trailer.read(4), "little")
        except FileNotFoundError:
            copy_size = 0
        if not 8 <= copy_size <= 0x1a2ff8:
            copy_size = 0x1a2ff8
        frame = (0x50000 + copy_size, 0x80000000,
                 ((copy_size - 1) % 0x2000) + 1)
        def assert_frame():
            raw = bytes.fromhex(packet("m202275f0,c"))
            assert tuple(int.from_bytes(raw[i:i + 4], "little")
                         for i in (0, 4, 8)) == frame
            assert int.from_bytes(bytes.fromhex(packet("m2021e080,4")), "little") == 0
            assert int.from_bytes(bytes.fromhex(packet("m2021e0a0,4")), "little") == 0x80000000 + copy_size
        packet("?")
        assert_frame()
        regs = registers()
        assert regs[15] == 0x80000414, hex(regs[15])
        assert regs[0] == 0xe000ed14, hex(regs[0])
        assert regs[1] == 0x80000415, hex(regs[1])
        assert regs[2] == 0x01000000, hex(regs[2])  # modeled CCR << 15
        assert regs[3:6] == [frame[0] - frame[2], 0x2021e080, 0x2000], regs[3:6]
        assert regs[6:12] == [0xa5a5a5a5] * 6, regs[6:12]
        assert regs[13] == 0x202275f0, hex(regs[13])  # inferred boot stack
        assert regs[14] == 0x2020e453, hex(regs[14])
        assert "QMP" in json.loads(qmp.readline())
        for name in ("qmp_capabilities", "system_reset"):
            if name == "system_reset":
                # Make PSP differ before reset, using guest MSR PSP, r4.
                for request in ("M801f0000,4:84f30988", "P4=78563412",
                                "Pf=00001f80"):
                    assert packet(request) == "OK", request
                assert packet("s").startswith("T05")
                assert packet("P0=00000000") == "OK"
                assert packet("P1=00000000") == "OK"
                assert packet("P2=00000000") == "OK"
                assert packet("P3=00000000") == "OK"
                assert packet("P5=00000000") == "OK"
                for reg in range(6, 12):
                    assert packet(f"P{reg:x}=00000000") == "OK"
                assert packet("Pd=00000000") == "OK"
                assert packet("Pe=00000000") == "OK"
                assert packet("M202275f0,c:000000000000000000000000") == "OK"
                assert packet("M2021e080,4:ffffffff") == "OK"
                assert packet("M2021e0a0,4:ffffffff") == "OK"
            qmp.write(json.dumps({"execute": name}).encode() + b"\n")
            while True:
                reply = json.loads(qmp.readline())
                if "error" in reply:
                    raise AssertionError(reply["error"])
                if "return" in reply:
                    break
        regs = registers()
        assert_frame()
        assert regs[:3] == [0xe000ed14, 0x80000415, 0x01000000], regs[:3]
        assert regs[3:6] == [frame[0] - frame[2], 0x2021e080, 0x2000], regs[3:6]
        assert regs[6:12] == [0xa5a5a5a5] * 6, regs[6:12]
        assert regs[13] == 0x202275f0, hex(regs[13])
        assert regs[14] == 0x2020e453, hex(regs[14])
        if mpu_test:
            # Confirm guest MRS observes both stack pointers before MAIN
            # replaces MSP. PSP retains the copied boot-task stack value.
            for request in ("M801f0000,8:eff30883eff30984", "Pf=00001f80"):
                assert packet(request) == "OK", request
            assert packet("s").startswith("T05")
            assert packet("s").startswith("T05")
            assert registers()[3:5] == [0x202275f0, 0x202275f0]
            # Inspect QEMU's initial masks with guest MRS instructions. The
            # physical values are inferred from task startup, not captured.
            for request in ("M801f0000,c:eff31080eff31381eff31182",
                            "Pf=00001f80"):
                assert packet(request) == "OK", request
            for _ in range(3):
                assert packet("s").startswith("T05")
            assert registers()[:3] == [0, 0, 0]
            # Exercise CCR through actual privileged guest instructions, not
            # only qtest/debugger register accesses. Cache data is not modeled.
            for request in ("M801f0000,4:01600268",  # str r1,[r0]; ldr r2,[r0]
                            "P0=14ed00e0", "P1=00020300", "Pf=00001f80"):
                assert packet(request) == "OK", request
            assert packet("s").startswith("T05")
            assert packet("s").startswith("T05")
            assert registers()[2] == 0x70200
            for request in ("P1=00000000", "Pf=00001f80"):
                assert packet(request) == "OK", request
            assert packet("s").startswith("T05")
            assert packet("s").startswith("T05")
            assert registers()[2] == 0x40200
            # Enter and return from SVC with a 4-byte-aligned MSP after a
            # guest attempts to clear STKALIGN. The exception frame must be
            # aligned and record the original alignment in stacked xPSR.
            svc_vector = packet("m8000002c,4")
            for request in ("M801f0000,2:00df",  # svc #0
                            "M801f0100,2:7047",  # bx lr
                            "M8000002c,4:01011f80", "Pd=04002120",
                            "Pf=00001f80"):
                assert packet(request) == "OK", request
            assert packet("s").startswith("T05")
            regs = registers()
            assert regs[15] == 0x801f0100, hex(regs[15])
            assert regs[13] == 0x2020ffe0, hex(regs[13])
            stacked_psr = int.from_bytes(bytes.fromhex(packet("m2020fffc,4")), "little")
            assert stacked_psr & 0x01000200 == 0x01000200, hex(stacked_psr)
            assert packet("s").startswith("T05")
            regs = registers()
            assert regs[13] == 0x20210004 and regs[15] == 0x801f0002, regs
            assert packet("M8000002c,4:" + svc_vector) == "OK"
            print(f"{image}: guest exception stack alignment and return passed")
            # Before MAIN executes, the inherited CPACR must permit an FP
            # register round trip without taking a UsageFault.
            for request in ("M801f0000,8:00ee100a10ee101a",  # vmov s0,r0; vmov r1,s0
                            "P0=78563412", "Pf=00001f80"):
                assert packet(request) == "OK", request
            assert packet("s").startswith("T05")
            assert packet("s").startswith("T05")
            assert registers()[1] == 0x12345678
            assert packet("Pf=14040080") == "OK"
            print(f"{image}: guest floating-point access at handoff passed")
            # With FP active and the inherited ASPEN/LSPEN bits set, exception
            # entry reserves the extended frame lazily and exception return
            # preserves the guest's FP register contents.
            for request in ("M801f0000,6:00df10ee101a",  # svc; vmov r1,s0
                            "M8000002c,4:01011f80", "Pd=04002120",
                            "P1=00000000", "Pf=00001f80"):
                assert packet(request) == "OK", request
            assert packet("s").startswith("T05")
            regs = registers()
            assert regs[15] == 0x801f0100 and regs[13] == 0x2020ff98, regs
            assert not regs[14] & 0x10, hex(regs[14])  # extended FP frame
            fpccr = int.from_bytes(bytes.fromhex(packet("me000ef34,4")), "little")
            fpcar = int.from_bytes(bytes.fromhex(packet("me000ef38,4")), "little")
            assert fpccr & 0xc0000001 == 0xc0000001, hex(fpccr)
            assert fpcar == 0x2020ffb8, hex(fpcar)
            assert packet("s").startswith("T05")
            assert registers()[13] == 0x20210004
            assert packet("s").startswith("T05")
            assert registers()[1] == 0x12345678
            assert not int.from_bytes(bytes.fromhex(packet("me000ef34,4")), "little") & 1
            assert packet("M8000002c,4:" + svc_vector) == "OK"
            print(f"{image}: guest lazy FP exception frame and return passed")
            for request in ("P0=14ed00e0", "P1=15040080", "P2=00000001",
                            "Pf=14040080"):
                assert packet(request) == "OK", request
            print(f"{image}: guest cache control enable and disable passed")
        with open(image, "rb") as firmware:
            main_msp = int.from_bytes(firmware.read(4), "little")
        inherited_msp = 0xdeadbee8  # Deliberately unmapped and aligned.
        assert main_msp != inherited_msp
        assert packet("Pd=" + inherited_msp.to_bytes(4, "little").hex()) == "OK"
        for instruction in range(6):
            assert packet("s").startswith("T05")
            regs = registers()
            assert regs[13] == (main_msp if instruction == 5 else inherited_msp)
        assert regs[15] == 0x80000422, hex(regs[15])
        print(f"{image}: MAIN reset handler replaces inherited MSP passed")
        if mpu_test:
            # Exercise the ROM's lifetime-counter access pattern with guest
            # LDR instructions, including the high-read latching the low half.
            # Hold MCR.MDIS so the programmed values are deterministic.
            for request in (
                "M40084000,4:02000000",  # PIT disabled
                "M40084108,4:00000000", "M40084118,4:00000000",
                "M40084100,4:78563412", "M40084110,4:f0debc9a",
                "M40084108,4:01000000", "M40084118,4:05000000",
                "M801f0000,4:10681968",  # ldr r0,[r2]; ldr r1,[r3]
                "P2=e0400840", "P3=e4400840", "Pf=00001f80",
            ):
                assert packet(request) == "OK", request
            assert packet("s").startswith("T05")
            assert packet("s").startswith("T05")
            assert registers()[:2] == [0x9abcdef0, 0x12345678]
            print(f"{image}: guest PIT lifetime-counter reads passed")
            # Reuse MAIN 1.10's region-10 attributes at a test RAM address.
            # A separate executable region covers the guest code and stack.
            # PRIVDEFENA must allow privileged background reads, while AP=3
            # permits unprivileged reads and writes inside region 10.
            for request in (
                "M20230000,2:0868",        # ldr r0,[r1]
                "M20230100,c:002383f31488bff36f8f7047",  # restore privilege; bx lr
                "M8000002c,4:01012320",    # SVC handler at 0x20230101
                "M80102000,4:78563412",
                "Me000ed98,4:09000000",    # region 9: code and stack
                "Me000ed9c,4:00002020",    # base 0x20200000
                "Me000eda0,4:25000303",    # 512 KiB, full access, normal RAM
                "Me000ed98,4:0a000000",    # region 10: MAIN's attributes
                "Me000ed9c,4:00001080",    # base 0x80100000
                "Me000eda0,4:27001003",    # MAIN 1.10 RASR 0x03100027
                "Me000ed94,4:05000000",    # MPU on, privileged background
                "P1=00000080", "Pd=00102320", "Pf=00002320",
            ):
                assert packet(request) == "OK", request
            assert packet("s").startswith("T05")
            assert registers()[0] == main_msp, (registers(), hex(main_msp))
            for request in (
                "M20230000,1a:62b683f31488bff36f8feff3148408680130086000dfeff31484",
                "P1=00201080", "P3=01000000", "Pf=00002320",
            ):
                assert packet(request) == "OK", request
            for _ in range(8):
                assert packet("s").startswith("T05")
            assert registers()[15] == 0x20230100, [hex(x) for x in registers()]
            assert registers()[4] & 1  # CONTROL.nPRIV was set in Thread mode
            assert int.from_bytes(bytes.fromhex(packet("m80102000,4")), "little") == 0x12345679
            for _ in range(4):
                assert packet("s").startswith("T05")
            assert registers()[15] == 0x20230016
            assert packet("s").startswith("T05")  # mrs r4,control
            assert not registers()[4] & 1  # SVC handler restored privilege
            assert packet("M8000002c,4:" + svc_vector) == "OK"
            if mpu_test == "privilege":
                # PRIVDEFENA must not grant an unprivileged data access to
                # SDRAM outside either enabled MPU region.
                for request in (
                    "M20230000,c:62b683f31488bff36f8f0868",
                    "M20230200,2:fee7", "M8000000c,4:01022320",
                    "P1=00002080", "P3=01000000", "Pf=00002320",
                ):
                    assert packet(request) == "OK", request
                for _ in range(4):
                    assert packet("s").startswith("T05")
                assert registers()[15] == 0x20230200, [hex(x) for x in registers()]
                assert int.from_bytes(bytes.fromhex(packet("me000ed28,4")), "little") & 0x82 == 0x82
                assert int.from_bytes(bytes.fromhex(packet("me000ed34,4")), "little") == 0x80200000
                print(f"{image}: unprivileged MPU background fault passed")
                return
            for request in ("Me000ed98,4:09000000", "Me000eda0,4:00000000",
                            "Me000ed98,4:0a000000", "Me000eda0,4:00000000"):
                assert packet(request) == "OK", request
            print(f"{image}: guest MAIN MPU permissions and background passed")
            if mpu_test == "execute":
                # An executable background region remains available for the
                # HardFault handler, while region 0 denies instruction fetch.
                for request in (
                    "M801f2000,4:013000bf",  # adds r0,#1; nop
                    "M801f0000,2:0868",      # ldr r0,[r1]
                    "M801f0100,2:fee7",      # b . (HardFault handler)
                    "M8000000c,4:01011f80",  # HardFault vector
                    "Me000ed98,4:00000000",  # region 0
                    "Me000ed9c,4:00201f80",  # base 0x801f2000
                    "Me000eda0,4:09000013",  # 32 bytes, full access, XN
                    "Me000ed28,4:ffffffff",  # clear prior fault status
                    "Me000ed94,4:05000000",  # MPU + privileged background
                    "P1=00201f80", "Pf=00001f80",
                ):
                    assert packet(request) == "OK", request
                assert packet("s").startswith("T05")
                assert registers()[0] == 0xbf003001  # data read is allowed
                for request in ("P0=00000000", "Pf=00201f80"):
                    assert packet(request) == "OK", request
                assert packet("s").startswith("T05")
                regs = registers()
                assert regs[15] == 0x801f0100 and regs[0] == 0, regs
                assert int.from_bytes(bytes.fromhex(packet("me000ed28,4")), "little") & 1
                print(f"{image}: guest MPU execute-never fault passed")
                return
            # A protected data region must affect guest accesses after MAIN
            # enables the MPU. Privileged background access keeps code executable.
            protected = 0x20000000 if mpu_test == "dtcm" else 0x801f2000
            for request in (
                "M801f0000,2:0868",      # ldr r0, [r1]
                "M801f0100,2:fee7",      # b . (HardFault handler)
                "M8000000c,4:01011f80",  # HardFault vector = 0x801f0101
                "Me000ed98,4:00000000",  # region 0
                "Me000ed9c,4:" + protected.to_bytes(4, "little").hex(),
                "Me000eda0,4:09000000",  # 32 bytes, no access, enabled
                "Me000ed94,4:05000000",  # MPU enabled, privileged default map
                "P1=" + protected.to_bytes(4, "little").hex(),
                "Pf=00001f80",
            ):
                assert packet(request) == "OK", request
            assert packet("s").startswith("T05")
            assert registers()[15] == 0x801f0100
            assert int.from_bytes(bytes.fromhex(packet("me000ed28,4")), "little") & 0x82 == 0x82
            assert int.from_bytes(bytes.fromhex(packet("me000ed34,4")), "little") == protected
            print(f"{image}: guest MPU protection and fault at {protected:#x} passed")
            return
        # Execute a Thumb LDR from unallocated FlexRAM, with a local HardFault
        # vector. Unlike qtest reads, this exercises the CPU's bus-fault path.
        for request in (
            "M801f0000,2:0868",      # ldr r0, [r1]
            "M801f0100,2:fee7",      # b . (HardFault handler)
            "M8000000c,4:01011f80",  # HardFault vector = 0x801f0101
            "M400b0014,4:" + error_bit.to_bytes(4, "little").hex(),
            "P1=" + invalid.to_bytes(4, "little").hex(),
            "Pf=00001f80",           # pc = test instruction
        ):
            assert packet(request) == "OK", request
        assert packet("s").startswith("T05")
        if error_bit == 0x20:
            regs = registers()
            assert regs[15] == 0x801f0002 and regs[0] == 0
            assert int.from_bytes(bytes.fromhex(packet("me000ed28,4")), "little") & 0x8200 == 0
        else:
            assert registers()[15] == 0x801f0100
            assert int.from_bytes(bytes.fromhex(packet("me000ed28,4")), "little") & 0x8200 == 0x8200
            assert int.from_bytes(bytes.fromhex(packet("me000ed38,4")), "little") == invalid
        assert int.from_bytes(bytes.fromhex(packet("m400b0010,4")), "little") == error_bit
        print(f"{image}: unallocated FlexRAM at {invalid:#x} produces " +
              ("a zero read" if error_bit == 0x20 else "a guest bus fault"))
    finally:
        qmp.close()
        qmp_host.close()
        host.close()
        process.terminate()
        process.wait(timeout=5)


def check_no_cache_selector(qemu, image):
    # L6 currently uses the generic QEMU M7 with no cache ID. CSSELR is
    # architecturally read-as-zero/write-ignored for this simulated profile.
    process = subprocess.Popen(
        [qemu, "-M", "l6max-main", "-kernel", image, "-accel", "qtest",
         "-qtest", "stdio", "-display", "none"], stdin=subprocess.PIPE,
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
        bufsize=1, env=isolated_env(),
    )
    try:
        for command, expected in (("readl 0xe000ed78", 0),
                                  ("writel 0xe000ed84 1", None),
                                  ("readl 0xe000ed84", 0),
                                  ("writel 0xe000ed14 0x30200", None),
                                  ("readl 0xe000ed14", 0x40200),
                                  ("writel 0xe000ed14 0", None),
                                  ("readl 0xe000ed14", 0x40200)):
            process.stdin.write(command + "\n")
            process.stdin.flush()
            if not select.select([process.stdout], [], [], 10)[0]:
                raise RuntimeError(f"no response to {command}")
            reply = process.stdout.readline().strip().split()
            assert reply[0] == "OK", reply
            if expected is not None:
                assert int(reply[1], 16) == expected, reply
        print(f"{image}: no-cache selector is read-as-zero/write-ignored")
    finally:
        process.terminate()
        process.wait(timeout=5)


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
        assert int(command("readl 0x400d8260").split()[1], 16) == 0x006c0001
        command("writel 0x400d8260 0xffffffff")
        command("writeb 0x400d8261 0xff")
        assert int(command("readl 0x400d8260").split()[1], 16) == 0x006c0001
        for address, expected in ((0x400f8004, 0), (0x400f801c, 0x02000009)):
            assert int(command(f"readl {address:#x}").split()[1], 16) == expected
            command(f"writel {address:#x} 0xffffffff")
            command(f"writeb {address + 1:#x} 0xff")
            assert int(command(f"readl {address:#x}").split()[1], 16) == expected
        # eDMA is a system bus master, not the CPU: its reads cannot access
        # private SCB registers even though privileged CPU reads can.
        command("writel 0x40384014 0x10000")
        command("writel 0x40384008 0x80000001")  # TE | FRDE
        command("writel 0xe000e100 0x10008")  # DMA completion + error IRQs
        command("writeb 0x400e8019 3")  # SEEI: enable channel 3 errors
        base = 0x400e9060  # channel 3
        for offset, value in ((0, 0xe000ed00), (4, 0x02020000),
                              (8, 4), (12, 0), (16, 0x40384020),
                              (20, 0x00010000), (24, 0), (28, 0x0001000a)):
            command(f"writel {base + offset:#x} {value:#x}")
        command("writel 0x400ec00c 0x80000014")
        command("writel 0x400e800c 8")
        # Handoff leaves the shared eDMA/DMAMUX gate off; a pending SAI
        # request must wait until firmware turns it on.
        gate = int(command("readl 0x400fc07c").split()[1], 16)
        assert not gate & 0xc0
        assert int(command("readl 0x400e802c").split()[1], 16) == 0
        command("writel 0x400d8154 0x40000000")  # stop selected crystal
        command(f"writel 0x400fc07c {gate | 0xc0:#x}")
        assert int(command("readl 0x400e802c").split()[1], 16) == 0
        command("writel 0x400d8158 0x40000000")  # restore IPG root
        assert not int(command("readl 0x400e8024").split()[1], 16) & 8
        assert not int(command("readl 0x40384040").split()[1], 16) & 0x1f0000
        assert int(command("readl 0x400e802c").split()[1], 16) == 8
        assert int(command("readl 0x400e8004").split()[1], 16) == 0x80000302
        assert int(command("readl 0xe000e200").split()[1], 16) & 0x10000
        assert int(command("readl 0x400e800c").split()[1], 16) == 8
        command("writel 0x400e800c 0")
        command("writel 0x400ec00c 0")
        command("writeb 0x400e801e 0x83")  # NOP must not clear
        assert int(command("readl 0x400e802c").split()[1], 16) == 8
        command("writeb 0x400e801e 3")  # CERR channel 3
        assert int(command("readl 0x400e802c").split()[1], 16) == 0
        assert not int(command("readl 0x400e8004").split()[1], 16) & 0x80000000
        command("writel 0xe000e280 0x10000")
        assert not int(command("readl 0xe000e200").split()[1], 16) & 0x10000
        command("writeb 0x400e8018 3")
        # RT1060 channels 3 and 19 share IRQ 3. Protect DTCM from CPU access;
        # system-bus DMA must still reach it through the TCM slave interface.
        command("writel 0xe000ed98 15")
        command("writel 0xe000ed9c 0x20000000")
        command("writel 0xe000eda0 0x21")  # 128 KiB, no CPU access
        command("writel 0xe000ed94 5")
        assert int(command("readl 0xe000ed94").split()[1], 16) == 5
        assert int(command("readl 0xe000eda0").split()[1], 16) == 0x21
        command("writel 0x20000000 0x12345678")
        for channel in (3, 19):
            base = 0x400e9000 + channel * 0x20
            for offset, value in ((0, 0x20000000), (4, 0x02020000),
                                  (8, 4), (12, 0), (16, 0x40384020),
                                  (20, 0x00010000), (24, 0),
                                  (28, 0x0001000a)):
                command(f"writel {base + offset:#x} {value:#x}")
            if channel == 3:
                command(f"writel 0x400fc07c {gate:#x}")  # stop DMA clock
            command(f"writel {0x400ec000 + channel * 4:#x} 0x80000014")
            command(f"writel 0x400e800c {1 << channel:#x}")
            if channel == 3:
                assert not int(command("readl 0x400e8024").split()[1], 16) & 8
                command(f"writel 0x400fc07c {gate | 0xc0:#x}")
            assert int(command("readl 0x400e8024").split()[1], 16) & (1 << channel)
            command(f"writel {0x400ec000 + channel * 4:#x} 0")
            command("writel 0x40384008 0x82000001")  # reset TX FIFO
        assert int(command("readl 0x400e802c").split()[1], 16) == 0
        assert int(command("readl 0x400e8024").split()[1], 16) & 0x80008 == 0x80008
        assert int(command("readl 0xe000e200").split()[1], 16) & 8
        assert not int(command("readl 0xe000e200").split()[1], 16) & (1 << 19)
        command("writel 0x400e8024 8")  # W1C channel 3 only
        assert int(command("readl 0x400e8024").split()[1], 16) & 0x80008 == 0x80000
        command("writel 0xe000e280 8")
        assert int(command("readl 0xe000e200").split()[1], 16) & 8
        command("writeb 0x400e801f 19")
        command("writel 0xe000e280 8")
        assert not int(command("readl 0xe000e200").split()[1], 16) & 8
        # Arm a SAI-paced transfer with TE clear, then enable TE through
        # the high byte. The already-active FIFO request must be serviced.
        command("writel 0x40384008 0x02000001")
        base = 0x400e9060
        for offset, value in ((0, 0x20000000), (4, 0x02020000),
                              (8, 4), (12, 0), (16, 0x40384020),
                              (20, 0x00010000), (24, 0), (28, 0x0001000a)):
            command(f"writel {base + offset:#x} {value:#x}")
        command("writel 0x400ec00c 0x80000014")
        command("writel 0x400e800c 8")
        assert not int(command("readl 0x400e8024").split()[1], 16) & 8
        command("writeb 0x4038400b 0x80")
        assert int(command("readl 0x400e8024").split()[1], 16) & 8
        # Fill RX while its DMA request is disabled, arm the channel, then
        # enable FRDE through the low byte and require immediate completion.
        command("writel 0x400e8024 8")
        command("writel 0x400e800c 0")
        command("writel 0x400ec00c 0")
        command("writel 0x40384008 0")
        command(f"writel 0x400fc07c {gate | 0x000c00c0:#x}")
        command("writel 0x40384010 0x05000000")
        command("writel 0x40384018 0x00070000")
        command("writel 0x4038401c 0x001f0000")
        command("writel 0x40384094 0x00010000")
        command("writel 0x40384088 0x80000000")
        command("clock_step 2000000")
        assert int(command("readl 0x40384088").split()[1], 16) & 0x10000
        command("writel 0x20000004 0xffffffff")
        for offset, value in ((0, 0x403840a0), (4, 0x02020000),
                              (8, 4), (12, 0), (16, 0x20000004),
                              (20, 0x00010000), (24, 0), (28, 0x0001000a)):
            command(f"writel {base + offset:#x} {value:#x}")
        command("writel 0x400ec00c 0x80000013")
        command("writel 0x400e800c 8")
        assert not int(command("readl 0x400e8024").split()[1], 16) & 8
        command("writeb 0x40384088 1")
        assert int(command("readl 0x400e8024").split()[1], 16) & 8
        assert int(command("readl 0x20000004").split()[1], 16) == 0
        qmp_execute("system_reset")
        assert int(command("readl 0x400d8260").split()[1], 16) == 0x006c0001
        assert int(command("readl 0x400f8004").split()[1], 16) == 0
        assert int(command("readl 0x400f801c").split()[1], 16) == 0x02000009
        print(f"{image}: paired DMA interrupt assertion and acknowledgement passed")
        # Bootloader installed and locked the NOR command table. Verify the
        # command works before any guest table write, then the key protocol.
        assert int(command("readl 0x402a801c").split()[1], 16) == 1
        assert int(command("readl 0x402a8200").split()[1], 16) == 0x0818040b
        mcr0 = int(command("readl 0x402a8000").split()[1], 16)
        command(f"writel 0x402a8000 {mcr0 | 1:#x}")
        assert int(command("readl 0x402a801c").split()[1], 16) == 1
        assert int(command("readl 0x402a8200").split()[1], 16) == 0x0818040b
        command("writel 0x402a80a0 0x50000")
        command("writel 0x402a80a4 4")
        command("writel 0x402a80b0 1")
        assert int(command("readl 0x402a8100").split()[1], 16) == int(
            command("readl 0x80000000").split()[1], 16)
        command("writel 0x402a8200 0x12345678")
        command("writeb 0x402a8201 0xaa")
        assert int(command("readl 0x402a8200").split()[1], 16) == 0x0818040b
        command("writel 0x402a8018 0")  # wrong key
        command("writel 0x402a801c 2")
        assert int(command("readl 0x402a801c").split()[1], 16) == 1
        command("writel 0x402a8018 0x5af05af0")
        command("writeb 0x402a8080 0")  # intervening controller write
        command("writel 0x402a801c 2")
        assert int(command("readl 0x402a801c").split()[1], 16) == 1
        command("writel 0x402a8018 0x5af05af0")
        command("writel 0x402a801c 2")
        assert int(command("readl 0x402a801c").split()[1], 16) == 2
        command("writel 0x402a8200 0x12345678")
        assert int(command("readl 0x402a8200").split()[1], 16) == 0x12345678
        command("writel 0x402a8018 0x5af05af0")
        command("writel 0x402a801c 1")
        command("writel 0x402a8200 0x87654321")
        assert int(command("readl 0x402a8200").split()[1], 16) == 0x12345678
        qmp_execute("system_reset")
        assert int(command("readl 0x402a8200").split()[1], 16) == 0x0818040b
        assert int(command("readl 0x402a801c").split()[1], 16) == 1
        print(f"{image}: FlexSPI boot LUT and lock protocol passed")
        # RT106x cache IDs are inferred from the documented M7 geometry.
        assert int(command("readl 0xe000ed80").split()[1], 16) == 0xf01fe019
        command("writel 0xe000ed84 0xf")
        assert int(command("readl 0xe000ed84").split()[1], 16) == 1
        assert int(command("readl 0xe000ed80").split()[1], 16) == 0xf03fe009
        command("writel 0xe000ed84 2")
        assert int(command("readl 0xe000ed84").split()[1], 16) == 0
        assert int(command("readl 0xe000ed80").split()[1], 16) == 0xf01fe019
        # Preserve cache enable state so guest initialization can read it
        # back, even though QEMU does not emulate cache contents.
        for bits in (0x10000, 0x20000, 0x30000, 0):
            command(f"writel 0xe000ed14 {0x200 | bits:#x}")
            assert int(command("readl 0xe000ed14").split()[1], 16) == 0x40200 | bits
        command("writel 0xe000ed14 0")
        assert int(command("readl 0xe000ed14").split()[1], 16) == 0x40200
        command("writel 0xe000ed14 0x30200")
        qmp_execute("system_reset")
        assert int(command("readl 0xe000ed14").split()[1], 16) == 0x40200
        print(f"{image}: cache identification, enable state and reset passed")
        # Aligned byte and halfword PIT accesses share the same timer state.
        command("writel 0x40084120 0x12345678")
        command("writeb 0x40084121 0xab")
        command("writew 0x40084122 0xcdef")
        assert int(command("readl 0x40084120").split()[1], 16) == 0xcdefab78
        assert int(command("readw 0x40084120").split()[1], 16) == 0xab78
        assert int(command("readb 0x40084123").split()[1], 16) == 0xcd
        command("writel 0x40084120 0")
        # The reconstructed handoff gate stops the ROM lifetime counter
        # even if firmware clears MCR.MDIS before enabling its CCGR field.
        assert int(command("readl 0x40084104").split()[1], 16) == 0xffffffff
        command("writel 0x40084000 0")
        command("clock_step 5000")
        assert int(command("readl 0x40084104").split()[1], 16) == 0xffffffff
        # PIT clock is PERCLK, gated independently from MCR. Use 24 MHz and
        # a 24-cycle period so each expiration is one microsecond.
        command("writel 0x40084108 0")
        command("writel 0x40084118 0")
        command("writel 0x40084100 23")
        command("writel 0x40084110 1")
        command("writel 0x4008410c 1")
        command("writel 0x4008411c 1")
        cscmr = int(command("readl 0x400fc01c").split()[1], 16)
        gate = int(command("readl 0x400fc06c").split()[1], 16)
        command(f"writel 0x400fc01c {(cscmr & ~0x7f) | 0x40:#x}")
        command(f"writel 0x400fc06c {(gate & ~0x3000) | 0x3000:#x}")
        command("writel 0x40084000 0")
        command("writeb 0x40084118 7")  # channel 1, chained + interrupt
        command("writeb 0x40084119 0xff")  # reserved lane preserves TCTRL
        assert int(command("readw 0x40084118").split()[1], 16) == 7
        command("writel 0x40084108 1")
        command("writel 0xe000e10c 0x04000000")
        command("clock_step 500")
        assert int(command("readl 0x40084104").split()[1], 16) == 11
        assert int(command("readl 0x400840e0").split()[1], 16) == 1
        command("clock_step 500")
        assert int(command("readl 0x400840e4").split()[1], 16) == 11
        assert int(command("readl 0x4008410c").split()[1], 16) == 1
        assert int(command("readl 0x40084114").split()[1], 16) == 0
        assert int(command("readl 0x4008411c").split()[1], 16) == 0
        command("clock_step 1000")
        assert int(command("readl 0x4008411c").split()[1], 16) == 1
        assert int(command("readl 0xe000e20c").split()[1], 16) & 0x04000000
        command("writeb 0x4008411d 0xff")  # upper byte must not clear flag
        assert int(command("readb 0x4008411c").split()[1], 16) == 1
        command("writeb 0x4008411c 1")
        command("writel 0xe000e28c 0x04000000")
        assert not int(command("readl 0xe000e20c").split()[1], 16) & 0x04000000
        command(f"writel 0x400fc06c {gate & ~0x3000:#x}")
        frozen = int(command("readl 0x40084104").split()[1], 16)
        command("clock_step 5000")
        assert int(command("readl 0x40084104").split()[1], 16) == frozen
        command(f"writel 0x400fc06c {(gate & ~0x3000) | 0x3000:#x}")
        command("clock_step 500")
        assert int(command("readl 0x40084104").split()[1], 16) == 11
        command("writel 0x40084000 2")
        command("clock_step 5000")
        assert int(command("readl 0x40084104").split()[1], 16) == 11
        # Handoff IPG is 150 MHz; PERCLK /3 gives 50 MHz. A new LDVAL
        # takes effect only on the next reload, not at the write.
        command("writel 0x40084108 0")
        command("writel 0x40084118 0")
        command("writel 0x40084100 149")
        command(f"writel 0x400fc01c {(cscmr & ~0x7f) | 2:#x}")
        command("writel 0x4008410c 1")
        command("writel 0x40084000 0")
        command("writel 0x40084108 1")
        command("clock_step 1000")
        assert int(command("readl 0x40084104").split()[1], 16) == 99
        command("writel 0x40084100 49")
        command(f"writel 0x400fc01c {(cscmr & ~0x7f) | 5:#x}")
        command("clock_step 2000")
        assert int(command("readl 0x40084104").split()[1], 16) == 49
        command(f"writel 0x400fc01c {(cscmr & ~0x7f) | 2:#x}")
        command("clock_step 1000")
        assert int(command("readl 0x4008410c").split()[1], 16) == 1
        assert int(command("readl 0x40084104").split()[1], 16) == 49
        command("clock_step 1000")
        assert int(command("readl 0x40084104").split()[1], 16) == 49
        cbcdr = int(command("readl 0x400fc014").split()[1], 16)
        command(f"writel 0x400fc014 {(cbcdr & ~0x300) | 0x100:#x}")
        command("clock_step 250")  # IPG /2, PERCLK /3: 100 MHz
        assert int(command("readl 0x40084104").split()[1], 16) == 24
        print(f"{image}: PIT clock sources, gate, chaining, reload and IRQ passed")
        qmp_execute("system_reset")
        # The handoff stopped SysTick but retained its 1 ms scheduler load.
        # Re-enable without reprogramming LOAD to exercise the retained value.
        assert int(command("readl 0xe000e014").split()[1], 16) == 599999
        command("writel 0xe000e018 0")
        command("writel 0xe000e010 5")
        command("clock_step 500000")
        remaining = int(command("readl 0xe000e018").split()[1], 16)
        assert 299998 <= remaining <= 300001, remaining
        command("writel 0xe000e010 0")
        for last, beyond in ((0x7ffc, 0x8000), (0x2001fffc, 0x20020000),
                             (0x202d7ffc, 0x202d8000)):
            command(f"writel {last:#x} 0x1234abcd")
            command(f"writel {beyond:#x} 0x5678efab")
            assert int(command(f"readl {last:#x}").split()[1], 16) == 0x1234abcd
            assert int(command(f"readl {beyond:#x}").split()[1], 16) != 0x5678efab
        print(f"{image}: FlexRAM mapping boundaries passed")
        # The controller latches enabled unallocated-access events, and
        # routes selected latched bits to shared IRQ 38 (NVIC word 1 bit 6).
        bad = (0x8000, 0x20020000, 0x202d8000)
        for address in bad:
            command(f"readl {address:#x}")
        assert int(command("readl 0x400b0010").split()[1], 16) == 0
        command("writew 0x400b0000 5")
        assert int(command("readl 0x400b0000").split()[1], 16) == 5
        command("writew 0x400b0002 0xffff")
        assert int(command("readw 0x400b0000").split()[1], 16) == 5
        command("writel 0x400b0000 0xffffffff")
        assert int(command("readl 0x400b0000").split()[1], 16) == 7
        command("writeb 0x400b0014 0xff")
        assert int(command("readl 0x400b0014").split()[1], 16) == 0x38
        assert int(command("readb 0x400b0014").split()[1], 16) == 0x38
        assert int(command("readw 0x400b0014").split()[1], 16) == 0x38
        command("writeb 0x400b0015 0xff")
        assert int(command("readl 0x400b0014").split()[1], 16) == 0x38
        command("writel 0xe000e284 0x40")
        for address in bad:
            command(f"readl {address:#x}")
        assert int(command("readl 0x400b0010").split()[1], 16) == 0x38
        assert not int(command("readl 0xe000e204").split()[1], 16) & 0x40
        command("writeb 0x400b0018 0x08")
        assert int(command("readl 0xe000e204").split()[1], 16) & 0x40
        command("writeb 0x400b0011 0xff")
        assert int(command("readb 0x400b0010").split()[1], 16) == 0x38
        command("writeb 0x400b0010 0x08")
        assert int(command("readw 0x400b0010").split()[1], 16) == 0x30
        command("writel 0xe000e284 0x40")
        assert not int(command("readl 0xe000e204").split()[1], 16) & 0x40
        command("writel 0x400b0018 0x30")
        assert int(command("readl 0xe000e204").split()[1], 16) & 0x40
        command("writel 0x400b0010 0x30")
        command("writel 0xe000e284 0x40")
        command("writel 0x400b0014 0x10")
        for address in bad:
            command(f"writel {address:#x} 0x12345678")
        assert int(command("readl 0x400b0010").split()[1], 16) == 0x10
        assert int(command("readl 0x202d8000").split()[1], 16) == 0
        command("writel 0x400b0010 0x38")
        command("writel 0x400b0014 0")
        command("writel 0x400b0018 0")
        command("writel 0xe000e284 0x40")
        print(f"{image}: FlexRAM access status and shared IRQ passed")
        # RC-derived 1 MHz output configuration has shared SET/CLR/TOG.
        assert int(command("readl 0x400d82c0").split()[1], 16) == 0x102e2
        command("writel 0x400d82c4 0x20001")
        assert int(command("readl 0x400d82c8").split()[1], 16) == 0x302e3
        command("writel 0x400d82cc 0x20001")
        assert int(command("readl 0x400d82c0").split()[1], 16) == 0x102e2
        command("writel 0x400d82c8 0x10000")
        assert int(command("readl 0x400d82c0").split()[1], 16) == 0x2e2
        command("writel 0x400d82c4 0x10000")
        # LOWPWR_CTRL aliases update writable fields while preserving the
        # read-only, initially ready crystal status and ignoring reserved bits.
        command("writel 0x400d8274 0x00010010")
        assert int(command("readl 0x400d8278").split()[1], 16) == 0x14011
        command("writel 0x400d8278 0x00010010")
        assert int(command("readl 0x400d827c").split()[1], 16) == 0x14001
        command("writel 0x400d827c 0x10")
        assert int(command("readl 0x400d8270").split()[1], 16) == 0x14011
        command("writel 0x400d8278 0x10")
        # MISC0 detector enable is writable through shared aliases; crystal
        # and bandgap status bits cannot be set by software.
        assert int(command("readl 0x400d8150").split()[1], 16) == 0x04000000
        command("writel 0x400d8154 0x00018080")
        assert int(command("readl 0x400d8158").split()[1], 16) == 0x04018000
        command("writel 0x400d815c 0x10000")
        assert int(command("readl 0x400d8150").split()[1], 16) == 0x04000000
        # SEMC SWRST clears itself without clearing configuration. STS0 is
        # read-only and idle because this model executes no timed commands.
        command("writel 0x402f0010 0x8000001b")
        command("writel 0x402f0000 0x1fff0005")
        assert int(command("readl 0x402f0000").split()[1], 16) == 0x1fff0004
        assert int(command("readl 0x402f0010").split()[1], 16) == 0x8000001b
        command("writel 0x402f00c0 0xffffffff")
        assert int(command("readl 0x402f00c0").split()[1], 16) == 1
        assert int(command("readb 0x402f00c0").split()[1], 16) == 1
        assert int(command("readb 0x402f00c1").split()[1], 16) == 0
        # Fractional PLL registers reserve bits 31:30 even for partial writes.
        for address in (0x400d8050, 0x400d8060, 0x400d8080,
                        0x400d8090, 0x400d80b0, 0x400d80c0):
            old = int(command(f"readl {address:#x}").split()[1], 16)
            command(f"writel {address:#x} 0xffffffff")
            assert int(command(f"readl {address:#x}").split()[1], 16) == 0x3fffffff
            command(f"writeb {address + 3:#x} 0xff")
            assert int(command(f"readl {address:#x}").split()[1], 16) == 0x3fffffff
            command(f"writel {address:#x} {old:#x}")
        # Observe the actual SysTick countdown in qtest virtual time. The
        # recovered 600 MHz ARM root halves when CACRR changes to divide by 4.
        command("writel 0xe000e010 0")
        command("writel 0xe000e014 100000")
        command("writel 0xe000e018 0")
        command("writel 0xe000e010 5")
        command("clock_step 1000")
        def systick_ticks(ns=10000):
            before = int(command("readl 0xe000e018").split()[1], 16)
            command(f"clock_step {ns}")
            after = int(command("readl 0xe000e018").split()[1], 16)
            return before - after
        assert 5998 <= systick_ticks() <= 6002
        command("writel 0x400fc010 3")
        assert 2998 <= systick_ticks() <= 3002
        command("writel 0x400d8008 0x2000")
        assert systick_ticks() == 0
        command("writel 0xe000e010 1")
        assert 9 <= systick_ticks(100000) <= 11
        command("writel 0xe000e010 5")
        assert systick_ticks() == 0
        command("writel 0x400d8004 0x2000")
        assert 2998 <= systick_ticks() <= 3002
        command("writel 0x400d8004 0x10000")
        assert 59 <= systick_ticks() <= 61  # 24 MHz bypass / 4
        command("writel 0x400d8008 0x10000")
        assert 2998 <= systick_ticks() <= 3002
        # The 100 kHz external reference survives CPU clock changes.
        command("writel 0xe000e010 1")
        assert 9 <= systick_ticks(100000) <= 11
        command("writel 0x400fc010 1")
        assert 9 <= systick_ticks(100000) <= 11
        command("writel 0xe000e010 5")
        assert 5998 <= systick_ticks() <= 6002
        command("writel 0x400d8000 0x2050")  # PLL1=960 MHz, root /2
        assert 4798 <= systick_ticks() <= 4802
        command("writel 0x400d8000 0x2064")
        assert 5998 <= systick_ticks() <= 6002
        # CALIB uses the modeled reference rate, but its physical value has
        # not been captured. A stopped input must not underflow the reload.
        assert int(command("readl 0xe000e01c").split()[1], 16) == 999
        # All modeled internal roots stop when their selected 24 MHz source
        # stops; selecting an enabled RC source restores both SysTick paths.
        command("writel 0x400d8154 0x40000000")  # power down selected XTAL
        assert int(command("readl 0xe000e01c").split()[1], 16) == 0x40000000
        assert not int(command("readl 0x400d8270").split()[1], 16) & 0x10000
        command("writel 0x400d8154 0x10000")
        assert not int(command("readl 0x400d8150").split()[1], 16) & 0x8000
        assert systick_ticks() == 0
        for address in (0x400d8000, 0x400d8010, 0x400d8030, 0x400d8070):
            assert not int(command(f"readl {address:#x}").split()[1], 16) & 0x80000000
        command("writel 0xe000e010 1")
        assert systick_ticks(100000) == 0
        command("writel 0x400d8274 0x10")  # select enabled 24 MHz RC
        assert int(command("readl 0xe000e01c").split()[1], 16) == 999
        for address in (0x400d8000, 0x400d8010, 0x400d8030, 0x400d8070):
            assert int(command(f"readl {address:#x}").split()[1], 16) & 0x80000000
        assert 9 <= systick_ticks(100000) <= 11
        command("writel 0xe000e010 5")
        assert 5998 <= systick_ticks() <= 6002
        command("writel 0x400d8278 1")  # disable selected RC
        assert systick_ticks() == 0
        command("writel 0x400d8274 1")
        assert 5998 <= systick_ticks() <= 6002
        command("writel 0x400d8158 0x40000000")
        assert int(command("readl 0x400d8270").split()[1], 16) & 0x10000
        assert int(command("readl 0x400d8150").split()[1], 16) & 0x8000
        command("writel 0x400d8158 0x10000")
        command("writel 0x400d8278 0x10")  # restore XTAL source
        # No external CLK1 reference is connected in this board model.
        for base in (0x400d8000, 0x400d8010, 0x400d8030, 0x400d8070, 0x400d80e0):
            assert int(command(f"readl {base:#x}").split()[1], 16) & 0x80000000
            command(f"writel {base + 4:#x} 0x4000")
            assert not int(command(f"readl {base:#x}").split()[1], 16) & 0x80000000
            command(f"writel {base + 8:#x} 0x4000")
            assert int(command(f"readl {base:#x}").split()[1], 16) & 0x80000000
        for base, power_on, power_off in ((0x400d8020, 4, 8), (0x400d80a0, 8, 4)):
            command(f"writel {base + power_on:#x} 0x1000")
            assert int(command(f"readl {base:#x}").split()[1], 16) & 0x80000000
            command(f"writel {base + 4:#x} 0x4000")
            assert not int(command(f"readl {base:#x}").split()[1], 16) & 0x80000000
            command(f"writel {base + 8:#x} 0x4000")
            command(f"writel {base + power_off:#x} 0x1000")
        # The ARM core uses the AHB root, not the PLL1 divider directly.
        boot_cbcdr = int(command("readl 0x400fc014").split()[1], 16)
        boot_cbcmr = int(command("readl 0x400fc018").split()[1], 16)
        command(f"writel 0x400fc014 {boot_cbcdr | (1 << 10):#x}")
        assert 2998 <= systick_ticks() <= 3002
        command(f"writel 0x400fc018 {boot_cbcmr & ~(3 << 18):#x}")
        assert 2638 <= systick_ticks() <= 2642  # PLL2 528 MHz / 2
        command("writel 0x400d8060 4")
        command("writel 0x400d8050 1")
        assert 2668 <= systick_ticks() <= 2672  # PLL2 534 MHz / 2
        command("writel 0x400d8050 0xffffffff")
        assert int(command("readl 0x400d8050").split()[1], 16) == 0x3fffffff
        assert 2608 <= systick_ticks() <= 2612  # signed -1 / 4 => 522 MHz / 2
        command("writel 0x400d8050 1")
        command(f"writel 0x400fc018 {(boot_cbcmr & ~(3 << 18)) | (1 << 18):#x}")
        assert 1655 <= systick_ticks() <= 1659  # PLL2 PFD2 / 2
        command("writel 0x400d8034 0x10000")  # bypass PLL2 and its PFD
        assert 118 <= systick_ticks() <= 122  # 24 MHz reference / 2
        command("writel 0x400d8038 0x2000")  # gate PLL2 outputs
        assert systick_ticks() == 0
        command("writel 0x400d8034 0x2000")
        command("writel 0x400d8038 0x10000")
        assert 1655 <= systick_ticks() <= 1659
        command("writel 0x400d8104 0x00800000")
        assert systick_ticks() == 0
        command("writel 0x400d8108 0x00800000")
        assert 1655 <= systick_ticks() <= 1659
        # Switch to the alternate 24 MHz oscillator, still with AHB /2.
        command(f"writel 0x400fc018 {(boot_cbcmr & ~(3 << 12)) | (1 << 12):#x}")
        command(f"writel 0x400fc014 {boot_cbcdr | (1 << 10) | (1 << 25):#x}")
        assert 118 <= systick_ticks() <= 122
        command("writel 0x400d8050 0")
        command("writel 0x400d8060 1")
        command(f"writel 0x400fc018 {boot_cbcmr:#x}")
        command(f"writel 0x400fc014 {boot_cbcdr:#x}")
        assert 5998 <= systick_ticks() <= 6002
        command("writel 0xe000e010 0")
        print(f"{image}: ARM clock, SysTick source and stopped clock passed")
        # SAI1 FIFO progress follows its CCGR gate and programmed root rate.
        # Align to the idle audio service's 1 ms virtual-clock cadence.
        now = int(command("clock_step 1").split()[1], 0)
        command(f"clock_step {1000000 - now % 1000000}")
        old_gate = int(command("readl 0x400fc07c").split()[1], 16)
        command("writel 0x400fc07c " + hex(old_gate & ~(3 << 18)))
        command("writel 0x40384010 0x05000000")  # master, MCLK1, /2
        command("writel 0x40384014 0x00010000")  # TX data line 0
        command("writel 0x40384018 0x00070000")  # eight slots
        command("writel 0x4038401c 0x001f0000")  # 32-bit words
        command("writel 0x40384008 0x80000000")  # TX enable
        def fill_sai():
            for i in range(16):
                command(f"writel 0x40384020 {i}")
            return int(command("readl 0x40384040").split()[1], 16) & 31
        before = fill_sai()
        command("clock_step 2000000")
        assert int(command("readl 0x40384040").split()[1], 16) & 31 == before
        command("writel 0x400fc07c " + hex(old_gate | (3 << 18)))
        command("clock_step 1400000")
        assert (int(command("readl 0x40384040").split()[1], 16) - before) & 31 == 16
        before = fill_sai()
        command("clock_step 1000000")
        assert int(command("readl 0x40384040").split()[1], 16) & 31 == before
        command("clock_step 400000")
        assert (int(command("readl 0x40384040").split()[1], 16) - before) & 31 == 16
        command("writel 0x400fc028 0x0ec102c3")  # double SAI1 PODF
        before = fill_sai()
        command("clock_step 2000000")
        assert int(command("readl 0x40384040").split()[1], 16) & 31 == before
        command("clock_step 800000")
        assert (int(command("readl 0x40384040").split()[1], 16) - before) & 31 == 16
        # Restore the SAI divider, then halve the audio PLL output through
        # MISC2_SET; the running scheduler must follow that alias write.
        command("writel 0x400fc028 0x0ec102c1")
        command("writel 0x400d8174 0x8000")
        command("clock_step 2800000")
        before = fill_sai()
        command("clock_step 2000000")
        assert int(command("readl 0x40384040").split()[1], 16) & 31 == before
        command("clock_step 800000")
        assert (int(command("readl 0x40384040").split()[1], 16) - before) & 31 == 16
        command("writel 0x400d8178 0x8000")
        command("clock_step 2800000")
        before = fill_sai()
        command("clock_step 1400000")
        assert (int(command("readl 0x40384040").split()[1], 16) - before) & 31 == 16
        # Route MCLK1 through PLL3 PFD2. Its bypass reference reaches SAI
        # only when the PLL output is enabled.
        old_cscmr1 = int(command("readl 0x400fc01c").split()[1], 16)
        command(f"writel 0x400fc01c {old_cscmr1 & ~(3 << 10):#x}")
        command("writel 0x400d8014 0x10000")  # bypass PLL3
        command("writel 0x400d8018 0x2000")  # gate PLL3 output
        command("clock_step 20000000")
        before = fill_sai()
        command("clock_step 20000000")
        assert int(command("readl 0x40384040").split()[1], 16) & 31 == before
        command("writel 0x400d8014 0x2000")
        command("clock_step 30000000")
        assert (int(command("readl 0x40384040").split()[1], 16) - before) & 31 == 16
        # MSEL=0 selects faster IPG; a high-byte write must reschedule the
        # still-pending slower PLL3 block without waiting for its deadline.
        before = fill_sai()
        command("writeb 0x40384013 0x01")
        command("clock_step 500000")
        assert (int(command("readl 0x40384040").split()[1], 16) - before) & 31 == 16
        command("writel 0x400d8018 0x10000")
        command(f"writel 0x400fc01c {old_cscmr1:#x}")
        # MCLK3 selects the internal SPDIF root. Route PLL4 through /4 and
        # /2, giving the same 24.576 MHz root and 48 kHz frame cadence.
        old_cdcdr = int(command("readl 0x400fc030").split()[1], 16)
        old_gpr1 = int(command("readl 0x400ac004").split()[1], 16)
        command(f"writel 0x400ac004 {old_gpr1 & ~(3 << 6):#x}")
        command(f"writel 0x400fc030 {(old_cdcdr & ~0x0ff00000) | (3 << 25) | (1 << 22):#x}")
        command("writel 0x40384010 0x0d000000")  # master, MCLK3, /2
        command("clock_step 2000000")
        before = fill_sai()
        command("clock_step 1400000")
        assert (int(command("readl 0x40384040").split()[1], 16) - before) & 31 == 16
        # External SPDIF input is unconnected; selecting it stops progress.
        command(f"writel 0x400ac004 {(old_gpr1 & ~(3 << 6)) | (1 << 6):#x}")
        command("clock_step 2000000")
        before = fill_sai()
        command("clock_step 2000000")
        assert int(command("readl 0x40384040").split()[1], 16) & 31 == before
        command("writel 0x40384008 0")
        command(f"writel 0x400ac004 {old_gpr1:#x}")
        command(f"writel 0x400fc030 {old_cdcdr:#x}")
        command("writel 0x400fc07c " + hex(old_gate))
        command("writel 0x400fc028 0x0ec102c1")
        print(f"{image}: SAI1 clock gate and divider drive FIFO cadence")
        # Reassign bank 0 from OCRAM to ITCM. Bank 7 moves to the next
        # ITCM address, while both physical banks retain their contents.
        command("writel 0x20280000 0x13579bdf")
        command("writel 0x00000000 0x2468ace0")
        command("writel 0x20200000 0x0badf00d")
        command("writel 0x400ac044 0x5555ea97")
        assert int(command("readl 0x00000000").split()[1], 16) == 0x13579bdf
        assert int(command("readl 0x00008000").split()[1], 16) == 0x2468ace0
        assert int(command("readl 0x20200000").split()[1], 16) == 0x0badf00d
        command("writel 0x400ac044 0x5555ea95")
        assert int(command("readl 0x20280000").split()[1], 16) == 0x13579bdf
        assert int(command("readl 0x00000000").split()[1], 16) == 0x2468ace0
        # Assign every FlexRAM bank to OCRAM: both TCM windows are empty.
        # Restoring the partition must retain the former ITCM bank's data.
        command("writel 0x400b0014 0x18")
        command("writel 0x400ac044 0x55555555")
        command("readl 0x00000000")
        command("readl 0x20000000")
        assert int(command("readl 0x400b0010").split()[1], 16) == 0x18
        command("writel 0x400b0010 0x18")
        command("writel 0x400b0014 0")
        command("writel 0x400ac044 0x5555ea95")
        assert int(command("readl 0x00000000").split()[1], 16) == 0x2468ace0
        # Source selection uses the assumed blank-fuse partition; writes to
        # GPR17 do not affect that partition until software selects it again.
        command("writel 0x400ac040 0x00200003")
        command("writel 0x400ac044 0xffffffff")
        command("writel 0x400ac00c 0xa5a5a5a5")
        command("writel 0x400ac028 0x5a5a5a5a")
        command("writel 0x402f0004 0xffffffff")
        for address in (0x402f0000, 0x402f0008, 0x402f000c,
                        0x402f0018, 0x402f0030, 0x402f0090,
                        0x402f0094, 0x402f0098, 0x402f00a0):
            command(f"writel {address:#x} 0x12345678")
        assert int(command("readl 0x20280000").split()[1], 16) == 0x13579bdf
        command("writel 0x400ac044 0x5555ea95")
        command("writel 0x400ac040 0x00200007")
        assert int(command("readl 0x00000000").split()[1], 16) == 0x2468ace0
        print(f"{image}: FlexRAM bank remapping and retained contents passed")
        main_vector = int(command("readl 0x80000000").split()[1], 16)
        command("writel 0x80000000 0x1234abcd")
        assert int(command("readl 0x80000000").split()[1], 16) == 0x1234abcd
        # Program four bytes in the emulated NOR through FlexSPI, leaving the
        # current SDRAM image alone until the next bootloaderless reset.
        main_word = int(command("readl 0x80000100").split()[1], 16)
        assert main_word != 0
        command("writel 0x402a8018 0x5af05af0")
        command("writel 0x402a801c 2")           # unlock LUT
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
        command("writel 0x400ac038 0xffffffff")
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
        # Audio/video PLL output enable is separate from PLL power and LOCK.
        command("writel 0x400d8078 0x2000")
        assert int(command("readl 0x400d8070").split()[1], 16) & 0x80000000
        command("writel 0x400d8074 0x2000")
        assert int(command("readl 0x400d8074").split()[1], 16) & 0x80000000
        command("writel 0x400d8074 0x1000")
        assert not int(command("readl 0x400d8070").split()[1], 16) & 0x80000000
        command("writel 0x400d8078 0x1000")
        assert int(command("readl 0x400d8078").split()[1], 16) & 0x80000000
        command("writel 0x400d807c 0x10000")
        assert int(command("readl 0x400d8070").split()[1], 16) & 0x10000
        command("writel 0x400d8070 0x80000000")
        assert int(command("readl 0x400d8070").split()[1], 16) == 0x80000000
        command("writel 0x400d80a0 0x1000")
        assert not int(command("readl 0x400d80a0").split()[1], 16) & 0x80000000
        command("writel 0x400d80a8 0x1000")
        assert int(command("readl 0x400d80a0").split()[1], 16) & 0x80000000
        command("writel 0x400d80a0 0x13000")
        for base in (0x400d80f0, 0x400d8100):
            original = int(command(f"readl {base:#x}").split()[1], 16)
            command(f"writel {base + 4:#x} 0x80")
            assert int(command(f"readl {base + 4:#x}").split()[1], 16) == original | 0x80
            command(f"writel {base + 8:#x} 0x80")
            assert int(command(f"readl {base:#x}").split()[1], 16) == original & ~0x80
            command(f"writel {base + 12:#x} 0x80")
            assert int(command(f"readl {base:#x}").split()[1], 16) == original ^ 0x80
            # A valid divider change is deferred while gated, then updates
            # the phase indicator when this model applies it on ungating.
            programmed = ((original & ~0x3f) | 0x80 |
                          ((original & 0x3f) + 1))
            command(f"writel {base:#x} {programmed:#x}")
            assert int(command(f"readl {base:#x}").split()[1], 16) == programmed
            command(f"writel {base + 8:#x} 0x80")
            applied = (programmed & ~0x80) ^ 0x40
            assert int(command(f"readl {base:#x}").split()[1], 16) == applied
            command(f"writel {base + 4:#x} 0x80")
            command(f"writel {base + 8:#x} 0x80")
            assert int(command(f"readl {base:#x}").split()[1], 16) == applied
            original = applied
            command(f"writel {base:#x} 0xffffffff")
            assert int(command(f"readl {base:#x}").split()[1], 16) == (
                (original & 0x40404040) | 0xbfbfbfbf
            )
        # USB PLL lock follows power independently of the output enable.
        command("writel 0x400d8018 0x2000")
        assert int(command("readl 0x400d8010").split()[1], 16) & 0x80000000
        command("writel 0x400d8014 0x2000")
        assert int(command("readl 0x400d8020").split()[1], 16) == 0x12000
        command("writel 0x400d8024 0x1040")
        assert int(command("readl 0x400d8024").split()[1], 16) == 0x80013040
        command("writel 0x400d8028 0x2000")
        assert int(command("readl 0x400d8020").split()[1], 16) == 0x80011040
        command("writel 0x400d8024 0x2000")
        command("writel 0x400d8028 0x1000")
        assert int(command("readl 0x400d8028").split()[1], 16) == 0x12040
        command("writel 0x400d802c 0x40")
        assert int(command("readl 0x400d802c").split()[1], 16) == 0x12000
        command("writel 0x400d8020 0x80000000")
        assert int(command("readl 0x400d8020").split()[1], 16) == 0
        # ARM and system PLL LOCK is read-only and independent of the
        # output enable; powerdown clears it in this immediate-lock model.
        for base, divider in ((0x400d8000, 0x64), (0x400d8030, 1)):
            assert int(command(f"readl {base:#x}").split()[1], 16) == (
                0x80002000 | divider
            )
            command(f"writel {base + 8:#x} 0x2000")
            assert int(command(f"readl {base + 4:#x}").split()[1], 16) == (
                0x80000000 | divider
            )
            command(f"writel {base + 4:#x} 0x80001000")
            assert int(command(f"readl {base:#x}").split()[1], 16) == 0x1000 | divider
            command(f"writel {base + 12:#x} 0x1000")
            assert int(command(f"readl {base:#x}").split()[1], 16) == (
                0x80000000 | divider
            )
            command(f"writel {base:#x} {0x80001000 | divider:#x}")
            assert int(command(f"readl {base:#x}").split()[1], 16) == 0x1000 | divider
        # Video control aliases share state; status follows the same
        # immediate powered-state approximation as the audio PLL.
        assert int(command("readl 0x400d80a0").split()[1], 16) == 0x13000
        command("writel 0x400d80a4 0x1e")
        command("writel 0x400d80a8 0x1000")
        assert int(command("readl 0x400d80a4").split()[1], 16) == 0x8001201e
        command("writel 0x400d80ac 0x1000")
        assert int(command("readl 0x400d80a0").split()[1], 16) == 0x1301e
        # ENET lock follows PLL power, independently of its output enable.
        assert int(command("readl 0x400d80e0").split()[1], 16) == 0x80312001
        command("writel 0x400d80e8 0x2000")
        assert int(command("readl 0x400d80ec").split()[1], 16) == 0x80310001
        command("writel 0x400d80e4 0x80001000")
        assert int(command("readl 0x400d80e4").split()[1], 16) == 0x00311001
        command("writel 0x400d80ec 0x1000")
        assert int(command("readl 0x400d80e0").split()[1], 16) == 0x80310001
        command("writel 0x400d80e0 0x80001000")
        assert int(command("readl 0x400d80e0").split()[1], 16) == 0x1000
        # MISC2 aliases share the register and cannot set PMU status or
        # reserved bits. The bootloader later clears both audio divider bits.
        assert int(command("readl 0x400d8170").split()[1], 16) == 0x00272727
        command("writel 0x400d8174 0x0058d858")
        assert int(command("readl 0x400d8174").split()[1], 16) == 0x0027a727
        command("writel 0x400d817c 0x00808000")
        assert int(command("readl 0x400d817c").split()[1], 16) == 0x00a72727
        command("writel 0x400d8178 0x00800000")
        assert int(command("readl 0x400d8178").split()[1], 16) == 0x00272727
        for address in (0x400d8020, 0x400d80a0, 0x400d80e0, 0x400d8160, 0x400d8170):
            command(f"writel {address:#x} 0xffffffff")
        command("writel 0x400d8010 0")
        assert not int(command("readl 0x400d8010").split()[1], 16) & 0x80000000
        command("writel 0x400d8070 0x00080000")
        command("writel 0x400d80b0 7")
        command("writel 0x400d80c0 13")
        command("writel 0x400d8040 0xffffffff")
        command("writel 0x400d82c0 0x00030fff")
        command("writel 0x402a8000 0xffffffff")
        command("writel 0x402a8004 0")
        command("writel 0x402a8008 0xffffffff")
        command("writel 0x402a800c 0xffffffff")
        for address in (0x402a8020, 0x402a8024, 0x402a8028, 0x402a802c,
                        0x402a8060, 0x402a8064, 0x402a8068, 0x402a806c,
                        0x402a8070, 0x402a8080, 0x402a8094, 0x402a80c0):
            command(f"writel {address:#x} 0xffffffff")
        command("writel 0x402a80b8 0xfffffffe")
        command("writel 0x402a80bc 0xfffffffe")
        # Dirty SEMC controls not rewritten by the bootloader, and leave an
        # incomplete FlexSPI page-program transaction in the controller.
        for address in (0x402f0034, 0x402f0038, 0x402f0058,
                        0x402f0060, 0x402f0084, 0x402f009c):
            command(f"writel {address:#x} 0xffffffff")
        # Earlier tests changed LUT 0; install the commands needed here.
        command("writel 0x402a8018 0x5af05af0")
        command("writel 0x402a801c 2")
        for address, value in ((0x402a8200, 0x0818040b),
                               (0x402a8210, 0x406),
                               (0x402a8240, 0x08180402),
                               (0x402a8250, 0x24040405)):
            command(f"writel {address:#x} {value:#x}")
        command("writel 0x402a8018 0x5af05af0")
        command("writel 0x402a801c 1")
        command("writel 0x402a80a0 0x50000")
        command("writel 0x402a80a4 8")  # LUT 0: NOR read
        command("writel 0x402a80b0 1")
        fifo = int(command("readl 0x402a80f0").split()[1], 16)
        assert fifo >= 1, (fifo, command("readl 0x402a8200"))
        command("writel 0x402a80a4 0x10000")  # LUT 1: write enable
        command("writel 0x402a80b0 1")
        command("writel 0x402a80a0 0x60000")
        command("writel 0x402a80a4 0x40008")  # LUT 4: program, 8 bytes
        command("writel 0x402a80b0 1")
        assert int(command("readl 0x402a8014").split()[1], 16) & 0x40
        command("writel 0x402a8010 0xffffffff")
        command("writel 0x400d8270 0x12345678")
        command("writel 0x400d8154 0x10000")
        command("writel 0x40080000 0x12345678")
        assert int(command("readl 0x40080000").split()[1], 16) == 0x92345678
        command("writel 0x4008000c 0x12345678")
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
        command("writel 0x400ac044 0xffffffff")
        for address in (0x40084000, 0x40084100, 0x40084108,
                        0x40084110, 0x40084118):
            command(f"writel {address:#x} 0x12345678")
        command("writel 0xe000e014 1")
        command("writel 0xe000ed20 0")
        command("writel 0xe000ed88 0")
        command("writel 0xe000ef34 0")
        command("writel 0x400b0014 0x38")
        command("writel 0x400b0018 0x38")
        for instance in range(8):
            base = 0x40184000 + instance * 0x4000
            command(f"writel {base:#x} 0xffffffff")
            command(f"writeb {base + 4:#x} 0xff")
            assert int(command(f"readl {base:#x}").split()[1], 16) == 0x04010003
            assert int(command(f"readl {base + 4:#x}").split()[1], 16) == 0x202
            command(f"writel {base + 0x10:#x} 0x12345678")
            command(f"writel {base + 0x20:#x} 0x12345678")
            assert int(command(f"readl {base + 0x10:#x}").split()[1], 16) == 0x12345678
            assert int(command(f"readl {base + 0x20:#x}").split()[1], 16) == 0x12345678
        # Dirty DMAMUX/eDMA controls and an implementation-unknown TCD.
        for address in (0x400ec000, 0x400ec030, 0x400ec07c):
            command(f"writel {address:#x} 0x8000007f")
        command("writel 0x400e800c 0x80001001")
        command("writel 0x400e8000 0xffffffff")
        command("writel 0x400e811c 0xffffffff")
        command("writel 0x400e9180 0x12345678")
        assert int(command("readl 0x400e800c").split()[1], 16) == 0x80001001
        # A system reset must discard SAI configuration and FIFO contents.
        command("writel 0x40384014 0x10000")
        command("writel 0x40384020 0x12345678")
        command("writel 0x40384010 0x01020304")
        command("writel 0x40384008 0x80000000")
        command("writel 0x40384094 0x10000")
        command("writel 0x40384088 0x80000000")
        assert int(command("readl 0x40384040").split()[1], 16) & 0x1f0000
        assert int(command("readl 0x40384008").split()[1], 16) & 0x80000000
        command("readl 0x20000000")
        assert int(command("readl 0x400b0010").split()[1], 16) == 0x10
        # Adapter-owned USDHC fields must reset with the controller.
        command("writel 0x402c002c 0x00123450")
        command("writel 0x402c00c8 0")
        command("writel 0x402c0028 0x07000000")
        qmp_execute("system_reset")
        for instance in range(8):
            base = 0x40184000 + instance * 0x4000
            assert int(command(f"readl {base + 0x10:#x}").split()[1], 16) == 0x0f000004
            assert int(command(f"readl {base + 0x20:#x}").split()[1], 16) == 0
        assert int(command("readl 0x402c002c").split()[1], 16) == 0x0080800f
        assert int(command("readl 0x402c00c8").split()[1], 16) == 0x00019006
        assert not int(command("readl 0x402c0028").split()[1], 16) & 0x07000000
        for address in (0x400ec000, 0x400ec030, 0x400ec07c,
                        0x400e800c, 0x400e8024):
            assert int(command(f"readl {address:#x}").split()[1], 16) == 0
        assert int(command("readl 0x400e8000").split()[1], 16) == 0x7f000400
        assert int(command("readl 0x400e8100").split()[1], 16) == 0x00010203
        assert int(command("readl 0x400e811c").split()[1], 16) == 0x1c1d1e1f
        # The reference manual marks the descriptor reset value unknown.
        assert int(command("readl 0x400e9180").split()[1], 16) == 0x12345678
        for address in (0x40384008, 0x40384010, 0x40384014,
                        0x40384040, 0x40384088, 0x40384094):
            assert int(command(f"readl {address:#x}").split()[1], 16) == 0
        assert int(command("readl 0x40384000").split()[1], 16) == 0x03000000
        assert int(command("readl 0x40384004").split()[1], 16) == 0x00050504
        for address, value in ((0xe000e014, 599999),
                               (0xe000ed20, 0xf0f00000),
                               (0xe000ed88, 0x00f00000),
                               (0xe000ef34, 0xc0000000)):
            assert int(command(f"readl {address:#x}").split()[1], 16) == value
        assert int(command("readl 0x400d8040").split()[1], 16) == 0
        assert int(command("readl 0x400d82c0").split()[1], 16) == 0x00010fff
        assert int(command("readl 0x400d8270").split()[1], 16) == 0x00054661
        assert int(command("readl 0x400d8150").split()[1], 16) == 0x04000000
        for address, value in ((0x402a8000, 0xffff1000),
                               (0x402a8004, 0xffffffff),
                               (0x402a8008, 0x200001f7),
                               (0x402a800c, 0),
                               (0x402a8020, 0x800f0000),
                               (0x402a8024, 0x800f0000),
                               (0x402a8028, 0x80000020),
                               (0x402a802c, 0x80000020),
                               (0x402a8060, 0x800),
                               (0x402a8064, 0), (0x402a8068, 0),
                               (0x402a806c, 0), (0x402a8070, 0x30063),
                               (0x402a8080, 0),
                               (0x402a8094, 1),
                               (0x402a80b8, 0),
                               (0x402a80bc, 0),
                               (0x402a80c0, 0x100)):
            assert int(command(f"readl {address:#x}").split()[1], 16) == value
        for address, expected in ((0x402f0034, 0x100), (0x402f0038, 0),
                                  (0x402f0058, 0x10410), (0x402f0060, 0),
                                  (0x402f0084, 0), (0x402f009c, 0),
                                  (0x402a8010, 0), (0x402a8014, 0),
                                  (0x402a80a0, 0), (0x402a80a4, 0),
                                  (0x402a80f0, 0), (0x402a8100, 0),
                                  (0x402a8180, 0)):
            assert int(command(f"readl {address:#x}").split()[1], 16) == expected
        command("writel 0x402a8014 0x40")
        assert int(command("readl 0x402a8014").split()[1], 16) == 0
        # Controller reset does not itself reset the external NOR latch.
        command("writel 0x402a80a4 0x50001")  # LUT 5: NOR status
        command("writel 0x402a80b0 1")
        assert int(command("readl 0x402a8100").split()[1], 16) & 2
        assert int(command("readl 0x40080000").split()[1], 16) == 0x92345678
        assert int(command("readl 0x4008000c").split()[1], 16) == 0x12345673
        assert int(command("readl 0x400ac00c").split()[1], 16) == 0xa5a5a5b5
        assert int(command("readl 0x400ac028").split()[1], 16) == 0x5a5a5a4a
        for address, value in ((0x402f0000, 0x1fff0004),
                               (0x402f0004, 8),
                               (0x402f0008, 0x81), (0x402f000c, 0x81),
                               (0x402f0018, 0), (0x402f0030, 0),
                               (0x402f0090, 0x82000000), (0x402f0094, 2),
                               (0x402f0098, 0), (0x402f00a0, 0x33)):
            assert int(command(f"readl {address:#x}").split()[1], 16) == value
        for address, value in ((0x40084000, 2), (0x40084100, 0xffffffff),
                               (0x40084108, 1), (0x40084110, 0xffffffff),
                               (0x40084118, 5)):
            assert int(command(f"readl {address:#x}").split()[1], 16) == value
        for address in (0x400b0000, 0x400b0010, 0x400b0014, 0x400b0018):
            assert int(command(f"readl {address:#x}").split()[1], 16) == 0
        assert int(command("readl 0x400ac044").split()[1], 16) == 0x5555ea95
        command("writel 0xe000e014 100000")
        command("writel 0xe000e018 0")
        command("writel 0xe000e010 5")
        command("clock_step 1000")
        assert 5998 <= systick_ticks() <= 6002
        command("writel 0xe000e010 0")
        assert int(command("readl 0x20280000").split()[1], 16) == 0x13579bdf
        assert int(command("readl 0x00000000").split()[1], 16) == 0x2468ace0
        assert int(command("readl 0x80000000").split()[1], 16) == main_vector & 0x20200000
        assert int(command("readl 0x80000100").split()[1], 16) == 0
        registers = qmp_execute("human-monitor-command", {"command-line": "info registers"})
        assert "R13=202275f0" in registers, registers
        assert int(command("readl 0x400ac038").split()[1], 16) == 0x00860000
        for address, expected in masks.items():
            assert int(command(f"readl {address:#x}").split()[1], 16) == expected
        # CCM and SEMC are reset by SRC cold reset; IOMUXC is retained.
        assert int(command("readl 0x400fc070").split()[1], 16) == 0x0c3f3033
        assert int(command("readl 0x400fc078").split()[1], 16) == 0x0000ffff
        assert int(command("readl 0x400fc06c").split()[1], 16) == 0xfc0cc000
        clock_handoff = {
            0x400fc000: 0x0401107f, 0x400fc00c: 0x100,
            0x400fc020: 0x13192e06, 0x400fc024: 0x06490b00,
            0x400fc02c: 0x007336c1, 0x400fc030: 0x33f71f92,
            0x400fc038: 0x00029150, 0x400fc060: 0x00120001,
        }
        for address, expected in clock_handoff.items():
            assert int(command(f"readl {address:#x}").split()[1], 16) == expected
        for address, value in ((0x400d8000, 0x80002064),
                               (0x400d8030, 0x80002001),
                               (0x400d8010, 0x80003040),
                               (0x400d8020, 0x12000), (0x400d80a0, 0x13000),
                               (0x400d80e0, 0x80312001),
                               (0x400d8070, 0x80002020),
                               (0x400d80b0, 0x05f5e100),
                               (0x400d80c0, 0x10a24447),
                               (0x400d8160, 0),
                               (0x400d8170, 0x00272727)):
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
            0x400fc068: 0xc0000fff, 0x400fc06c: 0xfc0cc000,
            0x400fc070: 0x0c3f3033, 0x400fc074: 0xf00ff330,
            0x400fc07c: 0xf0033c33, 0x400fc080: 0x00fc3fc0,
            0x400fc084: 0xfffffc33,
        }
        # A valid installed footer limits the next copy; bytes beyond it
        # retain SDRAM contents even when NOR holds different bytes.
        command("writel 0x402a8018 0x5af05af0")
        command("writel 0x402a801c 2")
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
        command("writel 0x400fc078 0xffffffff")  # dirty ROM-cleared gates
        qmp_execute("system_reset")
        assert int(command("readl 0x400fc078").split()[1], 16) == 0x0000ffff
        assert int(command("readl 0x80000100").split()[1], 16) == 0xdeadbeef
        for address, expected in ((0x202275f0, 0x50100),
                                  (0x202275f4, 0x80000000),
                                  (0x202275f8, 0x100),
                                  (0x2021e080, 0),
                                  (0x2021e0a0, 0x80000100)):
            assert int(command(f"readl {address:#x}").split()[1], 16) == expected
        regs = qmp_execute("human-monitor-command", {"command-line": "info registers"})
        for expected in ("R03=00050000", "R04=2021e080", "R05=00002000"):
            assert expected in regs, regs
        for address, expected in gate_results.items():
            assert int(command(f"readl {address:#x}").split()[1], 16) == expected
        print(f"{image}: masked GPR, gate and CPU reset passed")
    finally:
        qmp.close()
        qmp_host.close()
        process.terminate()
        process.wait(timeout=5)


def check_usb_clock(qemu, image):
    host, guest = socket.socketpair()
    host.settimeout(10)
    env = isolated_env()
    env["L6_USB_FD"] = str(guest.fileno())
    process = subprocess.Popen(
        [qemu, "-M", "l6max-main", "-kernel", image, "-accel", "qtest",
         "-qtest", "stdio", "-display", "none"], stdin=subprocess.PIPE,
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
        bufsize=1, pass_fds=(guest.fileno(),), env=env,
    )
    guest.close()

    def command(text):
        process.stdin.write(text + "\n")
        process.stdin.flush()
        if not select.select([process.stdout], [], [], 10)[0]:
            raise RuntimeError(f"no response to {text}")
        reply = process.stdout.readline().strip().split()
        assert reply[0] == "OK", reply
        return int(reply[1], 0) if len(reply) > 1 else None

    def request(kind, seq, ep, payload=b""):
        host.sendall(struct.pack("<5I", 0x4c365553, kind, seq, ep, len(payload)) + payload)
        reply = b""
        while len(reply) < 20:
            chunk = host.recv(20 - len(reply))
            if not chunk:
                raise EOFError("USB socket closed")
            reply += chunk
        magic, response, actual_seq, status, size = struct.unpack("<IIIiI", reply)
        assert (magic, response, actual_seq, size) == (0x4c365553, 0x80000000, seq, 0)
        return status

    try:
        assert command("readl 0x400fc080") & 3 == 0
        command("writel 0x402e0158 0x20200000")
        command("writel 0x402e0140 1")
        assert request(1, 1, 1) == 0  # cable connection is external
        command("writel 0x402e0144 0xffffffff")
        command("clock_step 2000000")
        assert not command("readl 0x402e0144") & 0x80
        assert request(2, 2, 0, bytes(8)) == -19  # controller clock stopped
        command("writel 0x400fc080 0x00fc3fc3")
        # Controller gate alone is insufficient while the PHY is in reset.
        command("clock_step 2000000")
        assert not command("readl 0x402e0144") & 0x80
        command("writel 0x400d9038 0xc0000000")  # release reset and clock
        assert command("readl 0x400d9030") == 0x00200000
        command("clock_step 2000000")
        assert command("readl 0x402e0144") & 0x80
        assert request(2, 3, 0, bytes(8)) == 0
        # PHY UTMI gate, PLL outputs and an unavailable PLL reference stop progress.
        for off, on, bit in ((0x400d9034, 0x400d9038, 0x40000000),
                             (0x400d8018, 0x400d8014, 0x40),
                             (0x400d8018, 0x400d8014, 0x1000),
                             (0x400d8014, 0x400d8018, 0x4000)):
            command("writel 0x402e0144 0xffffffff")
            command(f"writel {off:#x} {bit:#x}")
            command("clock_step 2000000")
            assert not command("readl 0x402e0144") & 0x80
            if bit == 0x4000:
                assert request(2, 300, 0, bytes(8)) == -19
            command(f"writel {on:#x} {bit:#x}")
            command("clock_step 2000000")
            assert command("readl 0x402e0144") & 0x80
        # USB PLL needs the selected oscillator even if its output is enabled.
        command("writel 0x402e0144 0xffffffff")
        command("writel 0x400d8154 0x40000000")  # stop selected crystal
        command("clock_step 2000000")
        assert not command("readl 0x402e0144") & 0x80
        command("writel 0x400d8274 0x10")  # select enabled RC source
        command("clock_step 2000000")
        assert command("readl 0x402e0144") & 0x80
        command("writel 0x400d8158 0x40000000")
        command("writel 0x400d8278 0x10")  # restore crystal source
        # The USB PLL can remain powered while its IPG bus root stops.
        command("writel 0x402e0144 0xffffffff")
        command("writel 0x400d8004 0x1000")  # power down selected ARM PLL
        command("clock_step 2000000")
        assert not command("readl 0x402e0144") & 0x80
        command("writel 0x400d8008 0x1000")
        command("clock_step 2000000")
        assert command("readl 0x402e0144") & 0x80
        command("writel 0x400d9034 0x80000000")
        assert command("readl 0x400d9030") == 0xc0200000
        command("writel 0x400d9038 0xc0000000")
        # USB is a system-bus DMA master; CPU-private PPB must not be
        # accessible, whereas the DTCM system-bus alias is accessible.
        command("writel 0x402e0158 0xe000e000")
        assert request(2, 4, 0, bytes(8)) == -14
        command("writel 0x402e0158 0x20000000")
        assert request(2, 5, 0, bytes.fromhex("0102030405060708")) == 0
        assert command("readl 0x20000028") == 0x04030201
        assert command("readl 0x2000002c") == 0x08070605
        command("writel 0x402e0158 0x00000000")
        assert request(2, 6, 0, bytes.fromhex("1122334455667788")) == 0
        assert command("readl 0x00000028") == 0x44332211
        assert command("readl 0x0000002c") == 0x88776655
        command("writel 0x402e0144 0xffffffff")
        command("writel 0x400fc080 0x00fc3fc0")
        command("clock_step 2000000")
        assert not command("readl 0x402e0144") & 0x80
        assert request(2, 7, 0, bytes(8)) == -19
        print(f"{image}: USB controller clock and DMA address-space checks passed")
    finally:
        host.close()
        process.terminate()
        process.wait(timeout=5)


def check_usdhc_clock(qemu, image):
    process = subprocess.Popen(
        [qemu, "-M", "l6max-main", "-kernel", image, "-accel", "qtest",
         "-qtest", "stdio", "-display", "none"], stdin=subprocess.PIPE,
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
        bufsize=1, env=isolated_env(),
    )

    def command(text):
        process.stdin.write(text + "\n")
        process.stdin.flush()
        if not select.select([process.stdout], [], [], 10)[0]:
            raise RuntimeError(f"no response to {text}")
        reply = process.stdout.readline().strip().split()
        assert reply[0] == "OK", reply
        return int(reply[1], 0) if len(reply) > 1 else None

    try:
        gate = command("readl 0x400fc080")
        assert not gate & 0xc
        command("writel 0x402c002c 0x00808000")  # initialize adapter clock
        command("writel 0x402c0034 0xffffffff")  # status enables
        command("writel 0x402c0030 0xffffffff")  # clear status
        assert not command("readl 0x402c0024") & 8
        command("writel 0x402c000c 0x08020000")  # CMD8 with response
        assert not command("readl 0x402c0030") & 0x18001
        # Select PLL2 PFD2, enable controller gate; an empty slot must still
        # produce a command timeout when a running controller is addressed.
        cscmr1 = command("readl 0x400fc01c")
        command(f"writel 0x400fc01c {cscmr1 & ~0x10000:#x}")
        command(f"writel 0x400fc080 {gate | 0xc:#x}")
        assert command("readl 0x402c0024") & 8
        command("writel 0x402c000c 0x08020000")
        assert command("readl 0x402c0030") & 0x10000
        command("writel 0x402c0030 0xffffffff")
        command("writel 0x400d8104 0x00800000")  # stop selected PFD2
        assert not command("readl 0x402c0024") & 8
        command("writel 0x402c000c 0x08020000")
        assert not command("readl 0x402c0030") & 0x18001
        command(f"writel 0x400fc01c {cscmr1 | 0x10000:#x}")  # PFD0
        assert command("readl 0x402c0024") & 8
        command("writel 0x400d8104 0x00000080")  # stop PFD0 too
        assert not command("readl 0x402c0024") & 8
        command("writel 0x400d8108 0x00000080")
        assert command("readl 0x402c0024") & 8
        command("writel 0x400d8004 0x1000")  # stop IPG, leave PLL2 on
        assert not command("readl 0x402c0024") & 8
        command("writel 0x400d8008 0x1000")
        assert command("readl 0x402c0024") & 8
        command("writel 0x400d8108 0x00800000")
        command("writel 0x402c000c 0x08020000")
        assert command("readl 0x402c0030") & 0x10000
        print(f"{image}: USDHC clock gate and PFD transitions passed")
    finally:
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
        0x401f8204: 0x10f0, 0x402f0000: 0x1fff0004,
        0x402f0004: 8, 0x402f0008: 0x81, 0x402f000c: 0x81,
        0x402f0018: 0, 0x402f0030: 0, 0x402f0010: 0x8000001b,
        0x402f0014: 0x8200001b, 0x402f0040: 0xf31,
        0x402f0044: 0x00664b22, 0x402f0048: 0x0009090b,
        0x402f004c: 0x08070a01, 0x400a8000: 5,
        0x402f0090: 0x82000000, 0x402f0094: 2,
        0x402f0098: 0, 0x402f00a0: 0x33,
        0x402f00c0: 1,
        0x402a8000: 0xffff1000, 0x402a8004: 0xffffffff,
        0x402a8008: 0x200001f7, 0x402a800c: 0, 0x402a801c: 1,
        0x402a8020: 0x800f0000, 0x402a8024: 0x800f0000,
        0x402a8028: 0x80000020, 0x402a802c: 0x80000020,
        0x402a8064: 0, 0x402a8068: 0, 0x402a806c: 0,
        0x402a80b8: 0, 0x402a80bc: 0,
        0x402a8200: 0x0818040b, 0x402a8204: 0x24043008,
        0x402a8210: 0x406, 0x402a8220: 0x08180420,
        0x402a8230: 0x081804d8, 0x402a8240: 0x08180402,
        0x402a8244: 0x2004, 0x402a8250: 0x24040405,
        0x402a8060: 0x800, 0x402a8070: 0x30063, 0x402a8080: 0,
        0x402a8094: 1, 0x402a80c0: 0x100,
        0x400ac00c: 0x0000f0f0, 0x400ac028: 7,
        0x400ac038: 0x00860000, 0x400ac040: 0x00200007,
        0x400ac044: 0x5555ea95,
        0x40084000: 2, 0x40084100: 0xffffffff, 0x40084108: 1,
        0x40084110: 0xffffffff, 0x40084118: 5,
        0x400fc068: 0xc0000fff, 0x400fc06c: 0xfc0cc000,
        0x400fc070: 0x0c3f3033, 0x400fc074: 0xf00ff330,
        0x400fc078: 0x0000ffff, 0x400fc07c: 0xf0033c33,
        0x400fc080: 0x00fc3fc0, 0x400fc084: 0xfffffc33,
        0x400fc000: 0x0401107f, 0x400fc00c: 0x100,
        0x400fc010: 1, 0x400fc014: 0x00098340, 0x400fc018: 0x7dae8324,
        0x400fc020: 0x13192e06, 0x400fc024: 0x06490b00,
        0x400fc02c: 0x007336c1, 0x400fc030: 0x33f71f92,
        0x400fc038: 0x00029150, 0x400fc060: 0x00120001,
        0x400d8000: 0x80002064, 0x400d8030: 0x80002001,
        0x400d8040: 0,
        0x400d82c0: 0x000102e2,
        0x400d8150: 0x04000000, 0x400d8270: 0x14001,
        0x40080000: 0x94030111, 0x4008000c: 0x113,
        0x400d8010: 0x80003040, 0x400d8020: 0x12000, 0x400d8070: 0x80002020,
        0x400d9000: 0x001e1c00, 0x400d9010: 0x10060607,
        0x400d9020: 0, 0x400d9030: 0xc0200000,
        # Simulation values assume the manual reset phase before the boot
        # divider writes; the device's inherited ROM phase is not captured.
        0x400d80f0: 0x13111061, 0x400d8100: 0x105d1058,
        0x400d8170: 0x00272727,
        0x400d80a0: 0x13000, 0x400d80b0: 0x05f5e100,
        0x400d80c0: 0x10a24447, 0x400d80e0: 0x80312001,
        0xe000e010: 0, 0xe000e014: 599999, 0xe000ed00: 0x411fc271, 0xe000ed08: 0x80000000,
        0xe000ed14: 0x40200, 0xe000ed20: 0xf0f00000,
        0xe000ed24: 0, 0xe000ed78: 0x09000003, 0xe000ed7c: 0x8303c003,
        0xe000ed80: 0xf01fe019, 0xe000ed84: 0,
        0xe000ed88: 0x00f00000, 0xe000ed94: 0,
        0xe000ef34: 0xc0000000,
    }
    lut_words = [0x0818040b, 0x24043008, 0, 0,
                 0x406, 0, 0, 0, 0x08180420, 0, 0, 0,
                 0x081804d8, 0, 0, 0, 0x08180402, 0x2004, 0, 0,
                 0x24040405, 0, 0, 0]
    l6max.update({0x402a8200 + 4 * i: value
                  for i, value in enumerate(lut_words)})
    check(qemu, stock, l6max)
    check(qemu, patched, l6max)
    check_main_stack_entry(qemu, stock, mpu_test=True)
    check_main_stack_entry(qemu, stock, mpu_test="privilege")
    check_main_stack_entry(qemu, stock, mpu_test="dtcm")
    check_main_stack_entry(qemu, stock, mpu_test="execute")
    check_main_stack_entry(qemu, stock)
    check_main_stack_entry(qemu, stock, 0x20020000, 0x10)
    check_main_stack_entry(qemu, stock, 0x202d8000, 0x20)
    check_main_stack_entry(qemu, patched)
    check_flexram_boundaries(qemu, stock)
    check_usb_clock(qemu, stock)
    check_usdhc_clock(qemu, stock)
    check_no_cache_selector(qemu, l6)
    check(qemu, l6, {0x400ac038: 0, 0x401f8204: 0, 0x402f0010: 0,
                     0x40084000: 0, 0x40084100: 0,
                     0x402a801c: 2, 0x402a8020: 0x80000020,
                     0x402a8064: 0x10000, 0x402a8200: 0,
                     0xe000e014: 0, 0xe000ed00: 0x411fc272, 0xe000ed20: 0,
                     0xe000ed88: 0})


if __name__ == "__main__":
    main()
