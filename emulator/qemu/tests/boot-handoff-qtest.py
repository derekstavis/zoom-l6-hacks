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


def check_main_stack_entry(qemu, image, invalid=0x8000, error_bit=0x08, mpu_test=False):
    # Bootloader 1.00 calls the reset handler with its current task stack.
    # MAIN 1.10's first six instructions install its own vector-table MSP
    # without accessing that inherited stack. Run those instructions with a
    # deliberately different MSP to verify this revision-specific property.
    host, guest = socket.socketpair()
    host.settimeout(10)
    process = subprocess.Popen(
        [qemu, "-M", "l6max-main", "-kernel", image, "-S", "-display", "none",
         "-chardev", f"socket,id=entry-gdb,fd={guest.fileno()}",
         "-gdb", "chardev:entry-gdb"],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL, pass_fds=(guest.fileno(),), env=isolated_env(),
    )
    guest.close()

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
        packet("?")
        regs = registers()
        assert regs[15] == 0x80000414, hex(regs[15])
        main_msp = regs[13]
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
            # A protected data region must affect guest accesses after MAIN
            # enables the MPU. Privileged background access keeps code executable.
            for request in (
                "M801f0000,2:0868",      # ldr r0, [r1]
                "M801f0100,2:fee7",      # b . (HardFault handler)
                "M8000000c,4:01011f80",  # HardFault vector = 0x801f0101
                "Me000ed98,4:00000000",  # region 0
                "Me000ed9c,4:00201f80",  # base = 0x801f2000
                "Me000eda0,4:09000000",  # 32 bytes, no access, enabled
                "Me000ed94,4:05000000",  # MPU enabled, privileged default map
                "P1=00201f80",
                "Pf=00001f80",
            ):
                assert packet(request) == "OK", request
            assert packet("s").startswith("T05")
            assert registers()[15] == 0x801f0100
            assert int.from_bytes(bytes.fromhex(packet("me000ed28,4")), "little") & 0x82 == 0x82
            assert int.from_bytes(bytes.fromhex(packet("me000ed34,4")), "little") == 0x801f2000
            print(f"{image}: guest MPU protection and fault passed")
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
        host.close()
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
        command("writel 0x400b0000 0xffffffff")
        assert int(command("readl 0x400b0000").split()[1], 16) == 7
        command("writel 0x400b0014 0xffffffff")
        assert int(command("readl 0x400b0014").split()[1], 16) == 0x38
        command("writel 0xe000e284 0x40")
        for address in bad:
            command(f"readl {address:#x}")
        assert int(command("readl 0x400b0010").split()[1], 16) == 0x38
        assert not int(command("readl 0xe000e204").split()[1], 16) & 0x40
        command("writel 0x400b0018 0x08")
        assert int(command("readl 0xe000e204").split()[1], 16) & 0x40
        command("writel 0x400b0010 0x08")
        assert int(command("readl 0x400b0010").split()[1], 16) == 0x30
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
        command("clock_step 1000000")
        assert (int(command("readl 0x40384040").split()[1], 16) - before) & 31 == 16
        before = fill_sai()
        command("clock_step 1000000")
        assert int(command("readl 0x40384040").split()[1], 16) & 31 == before
        command("clock_step 400000")
        assert (int(command("readl 0x40384040").split()[1], 16) - before) & 31 == 16
        command("writel 0x400fc028 0x0ec102c3")  # double SAI1 PODF
        command("clock_step 1400000")  # previous block finishes, new rate applies
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
        # MSEL=0 selects the IPG bus clock; it must drive SAI FIFO progress.
        command("writel 0x40384010 0x01000000")
        command("clock_step 2000000")
        before = fill_sai()
        command("clock_step 500000")
        assert (int(command("readl 0x40384040").split()[1], 16) - before) & 31 == 16
        command("writel 0x40384008 0")
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
        # USB2 PLL shares the RT1060 USB PLL control layout. LOCK is
        # synthesized only while powered and enabled, and aliases share state.
        assert int(command("readl 0x400d8020").split()[1], 16) == 0x12000
        command("writel 0x400d8024 0x1040")
        assert int(command("readl 0x400d8024").split()[1], 16) == 0x80013040
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
        # immediate power/enable approximation as the audio PLL.
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
        command("writel 0x400d8040 0xffffffff")
        command("writel 0x400d82c0 0x00030fff")
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
        command("writel 0x400b0014 0x38")
        command("writel 0x400b0018 0x38")
        command("readl 0x20000000")
        assert int(command("readl 0x400b0010").split()[1], 16) == 0x10
        qmp_execute("system_reset")
        assert int(command("readl 0x400d8040").split()[1], 16) == 0
        assert int(command("readl 0x400d82c0").split()[1], 16) == 0x00010fff
        assert int(command("readl 0x400ac00c").split()[1], 16) == 0xa5a5a5b5
        assert int(command("readl 0x400ac028").split()[1], 16) == 0x5a5a5a4a
        for address, value in ((0x402f0000, 0x1fff0004),
                               (0x402f0004, 0xffffffe8),
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
        assert "R13=20200000" in registers, registers
        assert int(command("readl 0x400ac038").split()[1], 16) == 0x00860000
        for address, expected in masks.items():
            assert int(command(f"readl {address:#x}").split()[1], 16) == expected
        assert int(command("readl 0x400fc070").split()[1], 16) == 0x0c000030
        assert int(command("readl 0x400fc078").split()[1], 16) == 0xc
        assert int(command("readl 0x400fc06c").split()[1], 16) == 0xcc0c4000
        for address, (keep, value) in clocks.items():
            assert int(command(f"readl {address:#x}").split()[1], 16) == keep | value
        for address, value in ((0x400d8000, 0x80002064),
                               (0x400d8030, 0x80002001),
                               (0x400d8010, 0x80003040),
                               (0x400d8020, 0x12000), (0x400d80a0, 0x13000),
                               (0x400d80e0, 0x80312001),
                               (0x400d8070, 0x80082020),
                               (0x400d8160, 0xffffffe0),
                               (0x400d8170, 0x3f2727a7)):
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
        0x401f8204: 0x10f0, 0x402f0000: 0x1fff0004,
        0x402f0004: 8, 0x402f0008: 0x81, 0x402f000c: 0x81,
        0x402f0018: 0, 0x402f0030: 0, 0x402f0010: 0x8000001b,
        0x402f0014: 0x8200001b, 0x402f0040: 0xf31,
        0x402f0044: 0x00664b22, 0x402f0048: 0x0009090b,
        0x402f004c: 0x08070a01, 0x400a8000: 5,
        0x402f0090: 0x82000000, 0x402f0094: 2,
        0x402f0098: 0, 0x402f00a0: 0x33,
        0x402f00c0: 1,
        0x400ac00c: 0x0000f0f0, 0x400ac028: 7,
        0x400ac038: 0x00860000, 0x400ac040: 0x00200007,
        0x400ac044: 0x5555ea95,
        0x40084000: 2, 0x40084100: 0xffffffff, 0x40084108: 1,
        0x40084110: 0xffffffff, 0x40084118: 5,
        0x400fc068: 0xc0000fff, 0x400fc06c: 0xfc0ccc00,
        0x400fc070: 0xfc3f303f, 0x400fc074: 0xffffff30,
        0x400fc078: 0xffffffff, 0x400fc07c: 0xf0033ff3,
        0x400fc080: 0xffff3fc3, 0x400fc084: 0xfffffc33,
        0x400fc000: 0x0401107f, 0x400fc00c: 0x100,
        0x400fc010: 1, 0x400fc014: 0x00098340, 0x400fc018: 0x7dae8324,
        0x400fc020: 0x13192e06, 0x400fc024: 0x06490b00,
        0x400fc02c: 0x007336c1, 0x400fc030: 0x33f71f92,
        0x400fc038: 0x00029150, 0x400fc060: 0x00120001,
        0x400d8000: 0x80002064, 0x400d8030: 0x80002001,
        0x400d8040: 0,
        0x400d82c0: 0x000102e2,
        0x400d8010: 0x80003040, 0x400d8020: 0x12000, 0x400d8070: 0x80002020,
        # Simulation values assume the manual reset phase before the boot
        # divider writes; the device's inherited ROM phase is not captured.
        0x400d80f0: 0x13111061, 0x400d8100: 0x105d1058,
        0x400d8170: 0x00272727,
        0x400d80a0: 0x13000, 0x400d80b0: 0x05f5e100,
        0x400d80c0: 0x10a24447, 0x400d80e0: 0x80312001,
        0xe000e010: 0, 0xe000ed00: 0x411fc271, 0xe000ed08: 0x80000000,
        0xe000ed14: 0x200, 0xe000ed24: 0, 0xe000ed94: 0,
    }
    check(qemu, stock, l6max)
    check(qemu, patched, l6max)
    check_main_stack_entry(qemu, stock, mpu_test=True)
    check_main_stack_entry(qemu, stock)
    check_main_stack_entry(qemu, stock, 0x20020000, 0x10)
    check_main_stack_entry(qemu, stock, 0x202d8000, 0x20)
    check_main_stack_entry(qemu, patched)
    check_flexram_boundaries(qemu, stock)
    check(qemu, l6, {0x400ac038: 0, 0x401f8204: 0, 0x402f0010: 0,
                     0x40084000: 0, 0x40084100: 0,
                     0xe000ed00: 0x411fc272})


if __name__ == "__main__":
    main()
