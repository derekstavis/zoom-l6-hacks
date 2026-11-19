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
    assert(((l6max_boot_clock_value(L6_CCM_ANALOG_PFD_528, 0) >> 16) & 63) == 29);
    uint32_t arm = l6max_boot_clock_value(L6_CCM_ANALOG_PLL_ARM, 0);
    uint32_t arm_div = l6max_boot_clock_value(L6_CCM_CACRR_OFFSET, 0);
    uint32_t bus = l6max_boot_clock_value(L6_CCM_CBCDR_OFFSET, 0);
    assert((arm & 0x7f) == 100 && (arm_div & 7) == 1);
    assert((bus & 0xc0) == 0x40 && ((bus >> 16) & 7) == 1);
    /* The post-divider is inherited, not established by the dump. */
    assert((l6max_boot_clock_value(L6_CCM_ANALOG_PLL_AUDIO, 0x80000) &
            0x180000) == 0x80000);

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
