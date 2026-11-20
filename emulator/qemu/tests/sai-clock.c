/* RT1060 SAI1 master-clock routing and fractional PLL behavior. */
#include "../hw/arm/l6-sai-clock.h"
#include "../hw/arm/l6-sai.h"
#include <assert.h>
#include <stdio.h>

int main(void)
{
    L6MaxSaiClocks c = {
        .cscmr1 = 2u << 10, .cs1cdr = (3u << 6) | 1u,
        .ccgr5 = 3u << 18, .audio_pll = 0x2020,
        .audio_num = 96, .audio_denom = 125,
        .usb1_pll = 0x3040, .pfd_480 = 17u << 16,
        .bus.oscillator_hz = 24000000u,
    };
    const uint32_t master1 = 1u << 26;
    assert(l6max_sai1_master_hz(&c, master1) == 24576000u);
    c.bus.oscillator_hz = 0;
    assert(l6max_sai1_master_hz(&c, master1) == 0);
    c.bus.oscillator_hz = 24000000u;
    assert(l6_sai_sample_rate(l6max_sai1_master_hz(&c, master1),
                              0, 7u << 16, 31u << 16) == 48000u);
    c.cs1cdr = (3u << 6) | 3u;
    assert(l6max_sai1_master_hz(&c, master1) == 12288000u);
    c.cs1cdr = (3u << 6) | 1u;
    c.misc2 = 1u << 15;
    assert(l6max_sai1_master_hz(&c, master1) == 12288000u);
    c.misc2 = 0;
    c.audio_pll = 0x102020; /* post-divider /1 */
    assert(l6max_sai1_master_hz(&c, master1) == 98304000u);
    c.audio_pll = 0x2020;
    c.audio_num = 0x3fffffffu; /* -1 */
    c.audio_denom = 4;
    assert(l6max_sai1_master_hz(&c, master1) == 23812500u);
    c.audio_denom = 0;
    assert(l6max_sai1_master_hz(&c, master1) == 0);
    c.audio_denom = 125;
    c.audio_num = 96;
    c.ccgr5 = 0;
    assert(l6max_sai1_master_hz(&c, master1) == 0);
    c.ccgr5 = 3u << 18;
    c.bus = (L6MaxBusClocks) {
        .oscillator_hz = 24000000u,
        .arm_pll = 0x2064, .cacrr = 1, .cbcdr = 0x00098340,
        .cbcmr = 3u << 18,
    };
    assert(l6max_sai1_master_hz(&c, 0) == 150000000u);
    c.bus.cbcdr |= 1u << 10;
    assert(l6max_sai1_master_hz(&c, 0) == 75000000u);
    c.gpr1 = 3;
    assert(l6max_sai1_master_hz(&c, master1) == 0);
    c.gpr1 = 0;
    c.cscmr1 = 0;
    assert(l6max_sai1_master_hz(&c, master1) ==
           (uint64_t)480000000u * 18 / 17 / 8);
    c.usb1_pll |= 0x10000u; /* PFD receives VCO even if main is bypassed. */
    assert(l6max_sai1_master_hz(&c, master1) ==
           (uint64_t)480000000u * 18 / 17 / 8);
    c.usb1_pll &= ~0x10000u;
    c.pfd_480 |= 1u << 23;
    assert(l6max_sai1_master_hz(&c, master1) == 0);
    c.pfd_480 &= ~(1u << 23);
    c.cscmr1 = 1u << 10;
    c.video_pll = 0x13000; /* bypass oscillator, PLL powered down */
    assert(l6max_sai1_master_hz(&c, master1) == 3000000u);
    c.video_pll = 0x2020;
    c.video_num = 96;
    c.video_denom = 125;
    assert(l6max_sai1_master_hz(&c, master1) == 24576000u);
    c.gpr1 = 1u << 3; /* MCLK2 selects SAI2 root */
    c.cscmr1 = 2u << 12;
    c.cs2cdr = (1u << 6) | 3u;
    assert(l6max_sai1_master_hz(&c, 2u << 26) == 24576000u);
    c.gpr1 = 1; /* MCLK1 selects SAI2 root */
    c.cscmr1 = 2u << 12;
    c.cs2cdr = (1u << 6) | 3u;
    assert(l6max_sai1_master_hz(&c, master1) == 24576000u);
    c.gpr1 = 0;
    c.cscmr1 = 2u << 10;
    c.audio_pll |= 0x1000;
    assert(l6max_sai1_master_hz(&c, master1) == 0);
    /* MCLK3 can receive the divided SPDIF root independently of the
     * external SPDIF clock inputs. Exercise all four root selectors. */
    c.audio_pll = 0x2020;
    c.gpr1 = 0;
    const uint32_t master3 = 3u << 26;
    c.cdcdr = 0x33f71f92u; /* reconstructed 1.00 handoff: PLL3 /2 /8 */
    assert(l6max_sai1_master_hz(&c, master3) == 30000000u);
    c.cdcdr = (3u << 25) | (1u << 22);
    assert(l6max_sai1_master_hz(&c, master3) == 24576000u);
    c.cdcdr |= 1u << 20;
    assert(l6max_sai1_master_hz(&c, master3) ==
           (uint64_t)480000000u * 18 / 17 / 8);
    c.cdcdr = (c.cdcdr & ~(3u << 20)) | (2u << 20);
    assert(l6max_sai1_master_hz(&c, master3) == 24576000u);
    c.cdcdr |= 1u << 20;
    assert(l6max_sai1_master_hz(&c, master3) == 60000000u);
    c.bus.ccsr |= 1u;
    assert(l6max_sai1_master_hz(&c, master3) == 3000000u);
    c.gpr1 = 1u << 6;
    assert(l6max_sai1_master_hz(&c, master3) == 0);
    c.gpr1 = 0;
    c.bus.oscillator_hz = 0;
    assert(l6max_sai1_master_hz(&c, master3) == 0);
    puts("L6max SAI1 clock routing passed");
}
