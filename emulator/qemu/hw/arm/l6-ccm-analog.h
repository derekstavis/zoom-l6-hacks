/* SPDX-License-Identifier: GPL-2.0-or-later */
/* CCM_ANALOG USB1 PLL control semantics shared by the L6 board model. */
#ifndef HW_ARM_L6_CCM_ANALOG_H
#define HW_ARM_L6_CCM_ANALOG_H

#include <stdint.h>

/* RT1040 RM Rev. 3 reset values for the clock registers needed to resolve
 * SAI1/SAI3 roots. The firmware's bootloader may override them before MAIN. */
#define L6_CCM_CSCMR1_OFFSET       0x0fc01c
#define L6_CCM_CACRR_OFFSET        0x0fc010
#define L6_CCM_CBCDR_OFFSET        0x0fc014
#define L6_CCM_CBCMR_OFFSET        0x0fc018
#define L6_CCM_CSCMR1_RESET        0x04900000u
#define L6_CCM_CS1CDR_OFFSET       0x0fc028
#define L6_CCM_CS1CDR_RESET        0x0ec102c1u
#define L6_CCM_CS2CDR_OFFSET       0x0fc02c
#define L6_CCM_CS2CDR_RESET        0x007336c1u
#define L6_CCM_ANALOG_PLL_SYS      0x0d8030
#define L6_CCM_ANALOG_PLL_ARM      0x0d8000
#define L6_CCM_ANALOG_PLL_SYS_NUM  0x0d8050
#define L6_CCM_ANALOG_PLL_SYS_DENOM 0x0d8060
#define L6_CCM_ANALOG_MISC2        0x0d8170
#define L6_CCM_ANALOG_PLL_SYS_RESET 0x00013001u
#define L6_CCM_ANALOG_PLL_AUDIO    0x0d8070
#define L6_CCM_ANALOG_PLL_AUDIO_RESET 0x00011006u
#define L6_CCM_ANALOG_PLL_AUDIO_NUM 0x0d8080
#define L6_CCM_ANALOG_PLL_AUDIO_NUM_RESET 0x05f5e100u
#define L6_CCM_ANALOG_PLL_AUDIO_DENOM 0x0d8090
#define L6_CCM_ANALOG_PLL_AUDIO_DENOM_RESET 0x2964619cu
#define L6_CCM_ANALOG_PFD_480 0x0d80f0
#define L6_CCM_ANALOG_PFD_480_RESET 0x13111018u
#define L6_CCM_ANALOG_PFD_528 0x0d8100
#define L6_CCM_ANALOG_PFD_528_RESET 0x1018101bu

/* Default L6max handoff, based on the recovered bootloader 1.00.
 * Only fields established by the recovered boot routine are changed; bits
 * inherited from earlier boot stages retain the board's existing seed. */
static inline uint32_t l6max_boot_clock_value(uint32_t offset, uint32_t old)
{
    switch (offset) {
    case L6_CCM_CACRR_OFFSET:
        return (old & ~7u) | 1u;
    case L6_CCM_CBCDR_OFFSET:
        return ((old & 0xc7f8e37fu) | 0x02010340u) & ~0x02000000u;
    case L6_CCM_CBCMR_OFFSET:
        return (((old & 0x1fff0fcfu) | 0x7c009320u) & 0xffffcfffu) | 0x000c0000u;
    case L6_CCM_CSCMR1_OFFSET:
        return ((old & 0xfc7f03c0u) | 0x6283a801u) & ~0x40u;
    case L6_CCM_CS1CDR_OFFSET:
        return (old & 0xfe00f000u) | 0xec102c1u;
    case L6_CCM_ANALOG_PLL_SYS:
        return (old & ~0x1f001u) | 0x80002001u;
    case L6_CCM_ANALOG_PLL_ARM:
        return (old & ~0x1f07fu) | 0x80002064u;
    case L6_CCM_ANALOG_PLL_SYS_NUM:
        return 0;
    case L6_CCM_ANALOG_PLL_SYS_DENOM:
        return 1;
    case L6_CCM_ANALOG_MISC2:
        return old & ~0x00808000u;
    case L6_CCM_ANALOG_PLL_AUDIO:
        return (old & 0xfffe0f80u) | 0x2020u;
    case L6_CCM_ANALOG_PLL_AUDIO_NUM:
        return 96;
    case L6_CCM_ANALOG_PLL_AUDIO_DENOM:
        return 125;
    case L6_CCM_ANALOG_PFD_528:
        return (old & 0x40404040u) | 0x101d1018u;
    case L6_CCM_ANALOG_PFD_480:
        return (old & 0x40404040u) | 0x13111021u;
    default:
        return old;
    }
}

#define L6_USB1_PLL_LOCK       (1u << 31)
#define L6_USB1_PLL_BYPASS     (1u << 16)
#define L6_USB1_PLL_ENABLE     (1u << 13)
#define L6_USB1_PLL_POWER      (1u << 12)
#define L6_USB1_PLL_BYPASS_SRC (3u << 14)
#define L6_USB1_PLL_EN_USB_CLKS (1u << 6)
#define L6_USB1_PLL_DIV_SELECT (1u << 1)
#define L6_USB1_PLL_WRITABLE   (L6_USB1_PLL_BYPASS | \
                                L6_USB1_PLL_BYPASS_SRC | \
                                L6_USB1_PLL_ENABLE | L6_USB1_PLL_POWER | \
                                L6_USB1_PLL_EN_USB_CLKS | \
                                L6_USB1_PLL_DIV_SELECT)

typedef enum L6Usb1PllAlias {
    L6_USB1_PLL_DIRECT,
    L6_USB1_PLL_SET,
    L6_USB1_PLL_CLEAR,
    L6_USB1_PLL_TOGGLE,
} L6Usb1PllAlias;

static inline uint32_t l6_usb1_pll_apply(uint32_t control, uint32_t value,
                                         L6Usb1PllAlias alias)
{
    value &= L6_USB1_PLL_WRITABLE;
    switch (alias) {
    case L6_USB1_PLL_DIRECT:
        return value;
    case L6_USB1_PLL_SET:
        return control | value;
    case L6_USB1_PLL_CLEAR:
        return control & ~value;
    case L6_USB1_PLL_TOGGLE:
        return control ^ value;
    }
    return control;
}

/* The modeled PLL locks once powered and enabled; its lock settles
 * immediately on the virtual clock. LOCK remains a read-only status bit. */
static inline uint32_t l6_usb1_pll_status(uint32_t control)
{
    control &= ~L6_USB1_PLL_LOCK;
    if ((control & (L6_USB1_PLL_POWER | L6_USB1_PLL_ENABLE)) ==
        (L6_USB1_PLL_POWER | L6_USB1_PLL_ENABLE)) {
        control |= L6_USB1_PLL_LOCK;
    }
    return control;
}

#endif /* HW_ARM_L6_CCM_ANALOG_H */
