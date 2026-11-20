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
    case 0x0fc000: /* CCR */
        return (old & 0xffffff00u) | 0x7fu;
    case 0x0fc00c: /* CCSR */
        return old & 0xfffffffeu;
    case L6_CCM_CACRR_OFFSET:
        return (old & ~7u) | 1u;
    case L6_CCM_CBCDR_OFFSET:
        return ((old & 0xc7f8e37fu) | 0x02010340u) & ~0x02000000u;
    case L6_CCM_CBCMR_OFFSET:
        return (((old & 0x1fff0fcfu) | 0x7c009320u) & 0xffffcfffu) | 0x000c0000u;
    case L6_CCM_CSCMR1_OFFSET:
        return ((old & 0xfc7f03c0u) | 0x6283a801u) & ~0x40u;
    case 0x0fc020: /* CSCMR2 */
        return (old & 0xfffffc03u) | 0x00180204u;
    case 0x0fc024: /* CSCDR1 */
        return (old & 0xfff8c780u) | 0x06010800u;
    case L6_CCM_CS1CDR_OFFSET:
        return (old & 0xfe00f000u) | 0xec102c1u;
    case L6_CCM_CS2CDR_OFFSET:
        return (old & 0xfffffe00u) | 0xc1u;
    case 0x0fc030: /* CDCDR */
        return (old & 0xf1ff8fffu) | 0x03f01f80u;
    case 0x0fc038: /* CSCDR2 */
        return old & 0xfe03ffffu;
    case 0x0fc060: /* CCOSR */
        return (old & 0xfe00fe00u) | 0x00120001u;
    case L6_CCM_ANALOG_PLL_SYS:
        return (old & ~0x1f001u) | 0x80002001u;
    case L6_CCM_ANALOG_PLL_ARM:
        return (old & ~0x1f07fu) | 0x80002064u;
    case L6_CCM_ANALOG_PLL_SYS_NUM:
        return 0;
    case 0x0d8040: /* PLL_SYS_SS: STOP, ENABLE and STEP from boot record. */
        return 0;
    case L6_CCM_ANALOG_PLL_SYS_DENOM:
        return 1;
    case L6_CCM_ANALOG_MISC2:
        return old & ~0xc0808000u;
    case 0x0d8160: /* MISC1 */
        return old & ~0x1fu;
    case 0x0d8020: /* PLL_USB2: disabled, bypassed */
        return 0x00012000u;
    case 0x0d80a0: /* PLL_VIDEO: powered down, bypassed */
        return 0x00013000u;
    case 0x0d80e0: /* PLL_ENET: locked, enabled, then bypass selected */
        return 0x80312001u;
    case L6_CCM_ANALOG_PLL_AUDIO:
        /* Startup waits for LOCK before clearing BYPASS. */
        return (old & 0xfffe0f80u) | 0x80002020u;
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

/* Model valid divider changes as taking effect immediately when ungated.
 * Gated changes are deferred; physical timing and inherited phase are unknown. */
#define L6_PFD_STABLE_MASK 0x40404040u
static inline uint32_t l6_pfd_apply(uint32_t control, uint32_t value,
                                    L6Usb1PllAlias alias, uint32_t *applied)
{
    uint32_t stable = control & L6_PFD_STABLE_MASK;
    uint32_t writable = control & ~L6_PFD_STABLE_MASK;
    value &= ~L6_PFD_STABLE_MASK;
    switch (alias) {
    case L6_USB1_PLL_DIRECT: writable = value; break;
    case L6_USB1_PLL_SET: writable |= value; break;
    case L6_USB1_PLL_CLEAR: writable &= ~value; break;
    case L6_USB1_PLL_TOGGLE: writable ^= value; break;
    }
    for (unsigned shift = 0; shift < 32; shift += 8) {
        uint32_t frac_mask = 0x3fu << shift;
        uint32_t frac = (writable >> shift) & 0x3fu;
        if (!(writable & (0x80u << shift)) && frac >= 12 && frac <= 35 &&
            ((writable ^ *applied) & frac_mask)) {
            stable ^= 0x40u << shift;
            *applied = (*applied & ~frac_mask) | (writable & frac_mask);
        }
    }
    return stable | writable;
}

/* RT1060 PLL_AUDIO/PLL_VIDEO controls share their writable bit layout. */
#define L6_AUDIO_PLL_WRITABLE 0x0019f07fu
static inline uint32_t l6_audio_pll_apply(uint32_t control, uint32_t value,
                                          L6Usb1PllAlias alias)
{
    control &= L6_AUDIO_PLL_WRITABLE;
    value &= L6_AUDIO_PLL_WRITABLE;
    switch (alias) {
    case L6_USB1_PLL_DIRECT: return value;
    case L6_USB1_PLL_SET: return control | value;
    case L6_USB1_PLL_CLEAR: return control & ~value;
    case L6_USB1_PLL_TOGGLE: return control ^ value;
    }
    return control;
}

static inline uint32_t l6_audio_pll_status(uint32_t control)
{
    control &= L6_AUDIO_PLL_WRITABLE;
    /* ENABLE gates the output; POWERDOWN controls the PLL itself. Lock
     * is approximated as immediate while powered, independent of ENABLE. */
    if (!(control & 0x1000u)) {
        control |= 0x80000000u;
    }
    return control;
}

/* CCM_ANALOG_MISC2 includes read-only PMU status and reserved bits. The
 * writable bits include audio/video dividers and PMU configuration fields. */
#define L6_MISC2_WRITABLE 0xffa7a7a7u
static inline uint32_t l6_misc2_apply(uint32_t control, uint32_t value,
                                      L6Usb1PllAlias alias)
{
    uint32_t preserved = control & ~L6_MISC2_WRITABLE;
    uint32_t writable = control & L6_MISC2_WRITABLE;
    value &= L6_MISC2_WRITABLE;
    switch (alias) {
    case L6_USB1_PLL_DIRECT: writable = value; break;
    case L6_USB1_PLL_SET: writable |= value; break;
    case L6_USB1_PLL_CLEAR: writable &= ~value; break;
    case L6_USB1_PLL_TOGGLE: writable ^= value; break;
    }
    return preserved | writable;
}

/* ARM and system PLL output ENABLE is separate from POWERDOWN. */
/* RT1060 ARM root from PLL1 and CACRR. UINT32_MAX means an unmodeled
 * external reference or an out-of-range divider; zero means no PLL output. */
static inline uint32_t l6max_arm_root_hz(uint32_t pll, uint32_t cacrr)
{
    const uint32_t oscillator_hz = 24000000u;
    uint64_t source;
    if (pll & 0x10000u) {
        if (pll & 0xc000u) {
            return UINT32_MAX; /* External CLK1 is not wired by this board. */
        }
        source = oscillator_hz;
    } else if ((pll & 0x3000u) != 0x2000u) {
        source = 0;
    } else {
        if (pll & 0xc000u) {
            return UINT32_MAX;
        }
        unsigned divider = pll & 0x7fu;
        if (divider < 54 || divider > 108) {
            return UINT32_MAX;
        }
        source = (uint64_t)oscillator_hz * divider / 2;
    }
    return source / ((cacrr & 7u) + 1);
}

/* RT1060 peripheral mux -> AHB -> IPG. Unknown means an unconnected
 * external reference, a reserved selection, or an invalid PLL setting. */
typedef struct L6MaxBusClocks {
    uint32_t arm_pll, cacrr, sys_pll, sys_num, sys_denom, pfd_528;
    uint32_t usb1_pll, ccsr, cbcdr, cbcmr;
    uint32_t oscillator_hz;
} L6MaxBusClocks;

static inline uint32_t l6max_sys_pll_vco_hz(const L6MaxBusClocks *c)
{
    uint32_t control = c->sys_pll;
    if (control & 0x1000u) {
        return 0;
    }
    if (control & 0xc000u) {
        return UINT32_MAX;
    }
    uint32_t denom = c->sys_denom & 0x3fffffffu;
    int64_t num = c->sys_num & 0x3fffffffu;
    if (num & (1u << 29)) {
        num -= 1u << 30;
    }
    if (!denom || num >= (int64_t)denom || num <= -(int64_t)denom) {
        return UINT32_MAX;
    }
    int64_t ratio = (int64_t)(20 + 2 * (control & 1u)) * denom + num;
    return (uint64_t)24000000u * ratio / denom;
}

static inline uint32_t l6max_sys_pll_hz(const L6MaxBusClocks *c)
{
    if (c->sys_pll & 0x10000u) {
        return (c->sys_pll & 0xc000u) ? UINT32_MAX : 24000000u;
    }
    return (c->sys_pll & 0x2000u) ? l6max_sys_pll_vco_hz(c) : 0;
}

static inline uint32_t l6max_pll2_pfd_hz(const L6MaxBusClocks *c, unsigned lane)
{
    unsigned shift = lane * 8;
    unsigned fraction = (c->pfd_528 >> shift) & 0x3fu;
    if (c->pfd_528 & (0x80u << shift)) {
        return 0;
    }
    uint32_t pll = l6max_sys_pll_vco_hz(c);
    if (!pll) {
        return 0;
    }
    if (pll == UINT32_MAX || fraction < 12 || fraction > 35) {
        return UINT32_MAX;
    }
    return (uint64_t)pll * 18 / fraction;
}

static inline uint32_t l6max_usb1_main_hz(uint32_t pll)
{
    if (pll & 0x10000u) {
        return (pll & 0xc000u) ? UINT32_MAX : 24000000u;
    }
    if ((pll & 0x3000u) != 0x3000u) {
        return 0;
    }
    return (pll & 0xc000u) ? UINT32_MAX :
           ((pll & 2u) ? 528000000u : 480000000u);
}

static inline uint32_t l6max_ahb_clock_hz(const L6MaxBusClocks *c)
{
    uint32_t source;
    if (c->cbcdr & (1u << 25)) {
        switch ((c->cbcmr >> 12) & 3u) {
        case 0:
            source = (c->ccsr & 1u) ?
                ((c->usb1_pll & 0xc000u) ? UINT32_MAX : 24000000u) :
                l6max_usb1_main_hz(c->usb1_pll);
            break;
        case 1: source = 24000000u; break;
        case 2: source = (c->sys_pll & 0xc000u) ? UINT32_MAX : 24000000u; break;
        default: return UINT32_MAX;
        }
        if (source == UINT32_MAX) {
            return source;
        }
        source /= ((c->cbcdr >> 27) & 7u) + 1;
    } else {
        switch ((c->cbcmr >> 18) & 3u) {
        case 0: source = l6max_sys_pll_hz(c); break;
        case 1: source = l6max_pll2_pfd_hz(c, 2); break;
        case 2: source = l6max_pll2_pfd_hz(c, 0); break;
        default: source = l6max_arm_root_hz(c->arm_pll, c->cacrr); break;
        }
    }
    if (source == UINT32_MAX) {
        return source;
    }
    return c->oscillator_hz ? source / (((c->cbcdr >> 10) & 7u) + 1) : 0;
}

static inline uint32_t l6max_ipg_clock_hz(const L6MaxBusClocks *c)
{
    uint32_t ahb = l6max_ahb_clock_hz(c);
    return ahb == UINT32_MAX ? ahb : ahb / (((c->cbcdr >> 8) & 3u) + 1);
}

#define L6_ARM_PLL_WRITABLE 0x0001f07fu
#define L6_SYS_PLL_WRITABLE 0x0001f001u
static inline uint32_t l6_core_pll_apply(uint32_t control, uint32_t value,
                                         L6Usb1PllAlias alias, uint32_t mask)
{
    control &= mask;
    value &= mask;
    switch (alias) {
    case L6_USB1_PLL_DIRECT: return value;
    case L6_USB1_PLL_SET: return control | value;
    case L6_USB1_PLL_CLEAR: return control & ~value;
    case L6_USB1_PLL_TOGGLE: return control ^ value;
    }
    return control;
}

/* Like ENET, assume instantaneous lock while powered; the manual does not
 * specify the actual time required for this transition. */
static inline uint32_t l6_core_pll_status(uint32_t control, uint32_t mask)
{
    control &= mask;
    return (control & 0x1000u) ? control : control | 0x80000000u;
}

/* RT1060 Ethernet PLL: output enables do not power the PLL itself. */
#define L6_ENET_PLL_WRITABLE 0x0031f00fu
static inline uint32_t l6_enet_pll_apply(uint32_t control, uint32_t value,
                                         L6Usb1PllAlias alias)
{
    control &= L6_ENET_PLL_WRITABLE;
    value &= L6_ENET_PLL_WRITABLE;
    switch (alias) {
    case L6_USB1_PLL_DIRECT: return value;
    case L6_USB1_PLL_SET: return control | value;
    case L6_USB1_PLL_CLEAR: return control & ~value;
    case L6_USB1_PLL_TOGGLE: return control ^ value;
    }
    return control;
}

/* Approximate the analog lock as immediate while powered; the three output
 * enables and bypass select affect outputs, not the PLL's power state. */
static inline uint32_t l6_enet_pll_status(uint32_t control)
{
    control &= L6_ENET_PLL_WRITABLE;
    return (control & 0x1000u) ? control : control | 0x80000000u;
}

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

/* RT1060 USB PLL ENABLE gates the output; POWER controls the PLL itself.
 * Model lock as immediate while powered, independent of that output gate. */
static inline uint32_t l6max_usb_pll_status(uint32_t control)
{
    control &= ~L6_USB1_PLL_LOCK;
    return (control & L6_USB1_PLL_POWER) ? control | L6_USB1_PLL_LOCK : control;
}

#endif /* HW_ARM_L6_CCM_ANALOG_H */
