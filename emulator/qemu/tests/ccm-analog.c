#include "../hw/arm/l6-ccm-analog.h"

#include <assert.h>
#include <stdio.h>

int main(void)
{
    uint32_t control = 0;

    /* The L6 board seeds the observed clock registers with RM reset values
     * instead of returning zero from the generic sparse-MMIO bank. */
    assert(L6_CCM_CSCMR1_RESET == 0x04900000u);
    assert(L6_CCM_CS1CDR_RESET == 0x0ec102c1u);
    assert(L6_CCM_ANALOG_PLL_SYS_RESET == 0x00013001u);
    assert(L6_CCM_ANALOG_PLL_AUDIO_RESET == 0x00011006u);
    assert(L6_CCM_ANALOG_PFD_480_RESET == 0x13111018u);
    assert(L6_CCM_ANALOG_PFD_528_RESET == 0x1018101bu);

    /* MISC2 aliases leave inherited read-only and reserved bits untouched. */
    uint32_t misc_status = 0x00585858u;
    assert(l6_misc2_apply(misc_status, 0xffffffffu, L6_USB1_PLL_DIRECT) ==
           0xffffffffu);
    assert(l6_misc2_apply(0xffffffffu, 0xffffffffu, L6_USB1_PLL_CLEAR) ==
           misc_status);
    assert(l6_misc2_apply(misc_status, misc_status, L6_USB1_PLL_TOGGLE) ==
           misc_status);

    /* The recovered L6max boot clock setup selects PLL4 for SAI1 and
     * configures its 32 + 96/125 multiplier and /4, /2 SAI dividers. */
    uint32_t mux = l6max_boot_clock_value(L6_CCM_CSCMR1_OFFSET,
                                          L6_CCM_CSCMR1_RESET);
    uint32_t div = l6max_boot_clock_value(L6_CCM_CS1CDR_OFFSET,
                                          L6_CCM_CS1CDR_RESET);
    assert(((mux >> 10) & 3) == 2);
    assert(((div >> 6) & 7) == 3 && (div & 63) == 1);
    assert(l6max_boot_clock_value(L6_CCM_ANALOG_PLL_AUDIO_NUM, 0) == 96);
    assert(l6max_boot_clock_value(L6_CCM_ANALOG_PLL_AUDIO_DENOM, 0) == 125);
    uint32_t pll = l6max_boot_clock_value(L6_CCM_ANALOG_PLL_AUDIO,
                                         L6_CCM_ANALOG_PLL_AUDIO_RESET);
    assert((pll & 0x7f) == 32 && (pll & 0x2000) && !(pll & 0x11000));
    assert(pll & 0x80000000u); /* successful boot waited for PLL lock */
    assert(((l6max_boot_clock_value(L6_CCM_ANALOG_PFD_528, 0) >> 16) & 63) == 29);
    uint32_t arm = l6max_boot_clock_value(L6_CCM_ANALOG_PLL_ARM, 0);
    uint32_t arm_div = l6max_boot_clock_value(L6_CCM_CACRR_OFFSET, 0);
    uint32_t bus = l6max_boot_clock_value(L6_CCM_CBCDR_OFFSET, 0);
    assert(l6max_arm_root_hz(arm, arm_div) == 600000000u);
    assert(l6max_arm_root_hz(arm, 3) == 300000000u);
    assert(l6max_arm_root_hz(0x00012064u, 1) == 12000000u);
    assert(l6max_arm_root_hz(0x00001064u, 1) == 0);
    assert(l6max_arm_root_hz(0x00006064u, 1) == UINT32_MAX);
    assert(l6max_arm_root_hz(0x00005064u, 1) == 0); /* powered down */
    assert(l6max_arm_root_hz(0x00014064u, 1) == UINT32_MAX); /* bypass CLK1 */
    assert(l6max_arm_root_hz(0x00002001u, 1) == UINT32_MAX);
    L6MaxBusClocks bus_clocks = {
        .oscillator_hz = 24000000u,
        .arm_pll = 0x2064, .cacrr = 1, .sys_pll = 0x2001,
        .sys_num = 0, .sys_denom = 1, .pfd_528 = 0x101d1018,
        .usb1_pll = 0x3040, .ccsr = 0x100,
        .cbcdr = 0x00098340, .cbcmr = 0x7dae8324,
    };
    assert(l6max_ahb_clock_hz(&bus_clocks) == 600000000u);
    assert(l6max_ipg_clock_hz(&bus_clocks) == 150000000u);
    bus_clocks.cbcdr = (bus_clocks.cbcdr & ~(7u << 10)) | (1u << 10);
    assert(l6max_ahb_clock_hz(&bus_clocks) == 300000000u);
    assert(l6max_ipg_clock_hz(&bus_clocks) == 75000000u);
    bus_clocks.cbcdr &= ~(7u << 10);
    bus_clocks.cbcmr &= ~(3u << 18);
    assert(l6max_ahb_clock_hz(&bus_clocks) == 528000000u);
    bus_clocks.sys_num = 0x3fffffffu; /* -1/4 of the 24 MHz reference */
    bus_clocks.sys_denom = 4;
    assert(l6max_ahb_clock_hz(&bus_clocks) == 522000000u);
    bus_clocks.sys_num = 0;
    bus_clocks.sys_denom = 1;
    bus_clocks.cbcmr |= 1u << 18;
    assert(l6max_ahb_clock_hz(&bus_clocks) == (uint64_t)528000000u * 18 / 29);
    bus_clocks.cbcmr = (bus_clocks.cbcmr & ~(3u << 18)) | (2u << 18);
    assert(l6max_ahb_clock_hz(&bus_clocks) == 396000000u);
    bus_clocks.sys_pll |= 0x10000u; /* PFD receives VCO, bypass affects PLL main. */
    assert(l6max_ahb_clock_hz(&bus_clocks) == 396000000u);
    bus_clocks.sys_pll &= ~0x10000u;
    bus_clocks.sys_pll = 0x5001; /* CLK1 selected but VCO powered down */
    assert(l6max_ahb_clock_hz(&bus_clocks) == 0);
    bus_clocks.sys_pll = 0x2001;
    bus_clocks.pfd_528 |= 0x80u;
    assert(l6max_ahb_clock_hz(&bus_clocks) == 0);
    bus_clocks.pfd_528 &= ~0x80u;
    bus_clocks.cbcdr |= 1u << 25;
    bus_clocks.cbcmr &= ~(3u << 12);
    assert(l6max_ahb_clock_hz(&bus_clocks) == 480000000u);
    bus_clocks.ccsr |= 1u; /* PLL3 switch selects bypass reference */
    assert(l6max_ahb_clock_hz(&bus_clocks) == 24000000u);
    bus_clocks.ccsr &= ~1u;
    bus_clocks.cbcmr |= 1u << 12;
    assert(l6max_ahb_clock_hz(&bus_clocks) == 24000000u);
    bus_clocks.cbcdr |= 1u << 27;
    assert(l6max_ahb_clock_hz(&bus_clocks) == 12000000u);
    bus_clocks.cbcmr = (bus_clocks.cbcmr & ~(3u << 12)) | (2u << 12);
    assert(l6max_ahb_clock_hz(&bus_clocks) == 12000000u); /* PLL2 bypass /2 */
    bus_clocks.cbcmr |= 1u << 12;
    assert(l6max_ahb_clock_hz(&bus_clocks) == UINT32_MAX);
    assert((arm & 0x7f) == 100 && (arm_div & 7) == 1);
    assert((bus & 0xc0) == 0x40 && ((bus >> 16) & 7) == 1);
    /* The post-divider is inherited, not established by the dump. */
    assert((l6max_boot_clock_value(L6_CCM_ANALOG_PLL_AUDIO, 0x80000) &
            0x180000) == 0x80000);

    uint32_t audio = l6_audio_pll_apply(0, 0x80002020u, L6_USB1_PLL_DIRECT);
    assert(audio == 0x2020u && (l6_audio_pll_status(audio) & 0x80000000u));
    audio = l6_audio_pll_apply(audio, 0x1000u, L6_USB1_PLL_SET);
    assert(!(l6_audio_pll_status(audio) & 0x80000000u));
    audio = l6_audio_pll_apply(audio, 0x1000u, L6_USB1_PLL_CLEAR);
    audio = l6_audio_pll_apply(audio, 0x10000u, L6_USB1_PLL_TOGGLE);
    assert((audio & 0x10000u) && (l6_audio_pll_status(audio) & 0x80000000u));

    /* Separate SET writes are cumulative; LOCK appears only after both the
     * documented POWER and ENABLE controls have been set. */
    control = l6_usb1_pll_apply(control, L6_USB1_PLL_POWER, L6_USB1_PLL_SET);
    assert(!(l6_usb1_pll_status(control) & L6_USB1_PLL_LOCK));
    control = l6_usb1_pll_apply(control, L6_USB1_PLL_ENABLE, L6_USB1_PLL_SET);
    assert(l6_usb1_pll_status(control) & L6_USB1_PLL_LOCK);

    /* CLR and TOG aliases update control state and deassert LOCK as soon as
     * the PLL is no longer both powered and enabled. LOCK is read-only. */
    control = l6_usb1_pll_apply(control, L6_USB1_PLL_ENABLE,
                                L6_USB1_PLL_CLEAR);
    assert(!(l6_usb1_pll_status(control) & L6_USB1_PLL_LOCK));
    control = l6_usb1_pll_apply(control, L6_USB1_PLL_ENABLE,
                                L6_USB1_PLL_TOGGLE);
    assert(l6_usb1_pll_status(control) & L6_USB1_PLL_LOCK);
    control = l6_usb1_pll_apply(control, L6_USB1_PLL_LOCK,
                                L6_USB1_PLL_DIRECT);
    assert(!(l6_usb1_pll_status(control) & L6_USB1_PLL_LOCK));

    puts("CCM_ANALOG USB1 PLL alias/status checks passed");
    return 0;
}
