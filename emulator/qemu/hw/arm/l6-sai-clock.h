/* SPDX-License-Identifier: GPL-2.0-or-later */
/* RT1060 SAI1 master-clock roots used by the L6max board. */
#ifndef HW_ARM_L6_SAI_CLOCK_H
#define HW_ARM_L6_SAI_CLOCK_H

#include <stdint.h>
#include "l6-ccm-analog.h"

typedef struct L6MaxSaiClocks {
    uint32_t gpr1, cscmr1, cs1cdr, cs2cdr, ccgr5;
    uint32_t audio_pll, audio_num, audio_denom;
    uint32_t video_pll, video_num, video_denom, misc2;
    uint32_t usb1_pll, pfd_480;
    L6MaxBusClocks bus;
} L6MaxSaiClocks;

/* Audio/video PLL post-divider is before bypass; MISC2 follows bypass. */
static inline uint32_t l6max_fractional_pll_hz(uint32_t control, uint32_t num,
                                                uint32_t denom, unsigned misc)
{
    static const unsigned post_div[] = { 4, 2, 1, 0 };
    static const unsigned misc_div[] = { 1, 2, 1, 4 };
    uint64_t hz;
    if (control & 0xc000u) {
        return 0; /* External CLK1 is not connected in this model. */
    }
    if (control & 0x10000u) {
        hz = 24000000u;
    } else {
        unsigned div = control & 0x7fu;
        unsigned post = post_div[(control >> 19) & 3];
        denom &= 0x3fffffffu;
        int64_t signed_num = num & 0x3fffffffu;
        if (signed_num & (1u << 29)) {
            signed_num -= 1u << 30;
        }
        int64_t ratio = (int64_t)div * denom + signed_num;
        if ((control & 0x3000u) != 0x2000u || !denom || !post ||
            div < 27 || div > 54 || ratio <= 0 ||
            signed_num >= (int64_t)denom || signed_num <= -(int64_t)denom) {
            return 0;
        }
        hz = (uint64_t)24000000u * ratio / denom / post;
    }
    return hz / misc_div[misc & 3];
}

static inline uint32_t l6max_usb1_pfd2_hz(uint32_t pll, uint32_t pfd)
{
    unsigned frac = (pfd >> 16) & 0x3fu;
    if ((pll & 0xc000u) || (pfd & 0x00800000u) || frac < 12 || frac > 35) {
        return 0;
    }
    /* The PFD is fed by PLL3's VCO, not the PLL output bypass mux. */
    if (!(pll & 0x1000u)) {
        return 0;
    }
    uint32_t source = (pll & 2u) ? 528000000u : 480000000u;
    return (uint64_t)source * 18 / frac;
}

static inline uint32_t l6max_sai1_master_hz(const L6MaxSaiClocks *c,
                                             uint32_t tcr2)
{
    /* SAI1 CCGR5 CG9: only 01 and 11 enable it in run mode. */
    if (!(c->ccgr5 & (1u << 18))) {
        return 0;
    }
    unsigned msel = (tcr2 >> 26) & 3;
    if (msel == 0) {
        uint32_t ipg = l6max_ipg_clock_hz(&c->bus);
        return ipg == UINT32_MAX ? 0 : ipg;
    }
    if (msel == 3) {
        return 0; /* SPDIF/external clock is not modeled. */
    }
    unsigned route = (c->gpr1 >> (msel == 1 ? 0 : 3)) & 7;
    if (route > 2) {
        return 0; /* The external MCLK input pins are not connected. */
    }
    unsigned mux = (c->cscmr1 >> (10 + 2 * route)) & 3;
    uint32_t source;
    switch (mux) {
    case 0:
        source = l6max_usb1_pfd2_hz(c->usb1_pll, c->pfd_480);
        break;
    case 1:
        source = l6max_fractional_pll_hz(c->video_pll, c->video_num,
                                          c->video_denom, c->misc2 >> 30);
        break;
    case 2:
        source = l6max_fractional_pll_hz(c->audio_pll, c->audio_num,
            c->audio_denom, ((c->misc2 >> 22) & 2) | ((c->misc2 >> 15) & 1));
        break;
    default:
        return 0;
    }
    uint32_t divreg = route == 1 ? c->cs2cdr : c->cs1cdr;
    unsigned shift = route == 2 ? 16 : 0;
    unsigned pred = ((divreg >> (shift + 6)) & 7) + 1;
    unsigned podf = ((divreg >> shift) & 63) + 1;
    return source / pred / podf;
}

#endif /* HW_ARM_L6_SAI_CLOCK_H */
