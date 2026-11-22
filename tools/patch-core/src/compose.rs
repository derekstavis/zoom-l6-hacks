//! Places static payloads in the MAIN image and chains the hooks they share.
//! `l6patch` builds the units from payloads it compiles; the browser patcher
//! builds them from precompiled templates. Both then compose them here, so
//! they produce the same image.
use std::collections::{BTreeMap, BTreeSet};

use crate::{HookEdit, PatchEdits, apply_patch_with_map, link_identity_ring};

/// Where a hook continues once its handler returns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Continuation {
    /// The hooked function or call site. Hooks chain only on the same site.
    pub site: String,
    /// Offset in the payload of the word holding the next handler's address.
    pub slot: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitHook {
    pub address: u32,
    pub target: u32,
    /// Bytes the branch to `target` displaces.
    pub length: usize,
    pub continuation: Option<Continuation>,
}

/// One static payload, linked for `base`.
#[derive(Clone, Debug)]
pub struct Unit {
    pub name: String,
    pub base: u32,
    pub payload: Vec<u8>,
    pub hooks: Vec<UnitHook>,
    /// Address of the payload's identity record.
    pub identity: u32,
}

/// One of the Patch loader's hooks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoaderHook {
    pub address: u32,
    pub target: u32,
    /// Bytes the branch to `target` displaces.
    pub length: usize,
    /// A dispatch site, where SD modules attach at runtime. A static patch
    /// may take over a static hook only at the same address and length.
    pub dispatch: bool,
}

/// The Patch loader, placed before every static payload.
#[derive(Clone, Debug)]
pub struct Loader {
    pub base: u32,
    pub payload: Vec<u8>,
    pub hooks: Vec<LoaderHook>,
    /// The loader's identity record.
    pub identity: u32,
    /// Start of the SD module region that static payloads must not reach.
    pub module_base: u32,
    /// End of the reserved region the image grows to.
    pub reserve_end: u32,
}

/// The fixed facts about the image the units go into.
pub struct Image<'a> {
    pub firmware: &'a mut Vec<u8>,
    /// MAIN's length before any payload.
    pub original_length: usize,
    /// The stock erased padding after MAIN.
    pub padding: &'a [u8],
    pub firmware_base: u32,
    pub slot_end: u32,
}

/// What composing one unit did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    pub base: u32,
    pub payload_len: usize,
    /// Indices into the unit's hooks of those it installed. A hook on a site
    /// an earlier hook already holds is reached through the chain instead.
    pub installed: Vec<usize>,
}

/// Place the loader, if any, chain shared hooks, place every unit in order,
/// link the identity ring and, with a loader, grow the image to its reserve.
pub fn compose(
    image: Image<'_>,
    loader: Option<&Loader>,
    mut units: Vec<Unit>,
) -> Result<Vec<Placed>, String> {
    // A patch hooking one of the loader's sites continues into the loader.
    let mut targets = BTreeMap::new();
    let mut loader_ranges = Vec::new();
    if let Some(loader) = loader {
        let overlap = |a: &LoaderHook, b: &LoaderHook| {
            a.address < b.address + b.length as u32 && b.address < a.address + a.length as u32
        };
        for (index, hook) in loader.hooks.iter().enumerate() {
            if loader.hooks[..index]
                .iter()
                .any(|prior| overlap(prior, hook))
            {
                return Err(format!(
                    "Patch loader hook at {:#x} overlaps another of its hooks",
                    hook.address
                ));
            }
            targets.insert(hook.address, hook.target | 1);
            if !hook.dispatch {
                loader_ranges.push((hook.address, hook.length));
            }
        }
        let edits = PatchEdits::new(
            loader
                .hooks
                .iter()
                .map(|hook| HookEdit {
                    address: hook.address,
                    target: hook.target,
                    length: hook.length,
                })
                .collect(),
        );
        apply_patch_with_map(
            image.firmware,
            image.original_length,
            loader.base,
            &loader.payload,
            &edits,
            image.padding,
            image.firmware_base,
            loader.module_base,
        )
        .map_err(|error| format!("Patch loader: {error}"))?;
    }
    chain(&mut units, loader.map(|_| &targets))?;

    let mut claimed = loader_ranges.clone();
    let mut installed_sites = BTreeSet::new();
    let mut placed = Vec::with_capacity(units.len());
    for unit in &units {
        let mut installed = Vec::new();
        for (index, hook) in unit.hooks.iter().enumerate() {
            if !installed_sites.insert(hook.address) {
                continue;
            }
            let overlaps = |&&(address, length): &&(u32, usize)| {
                hook.address < address + length as u32
                    && address < hook.address + hook.length as u32
            };
            if let Some(&prior) = claimed.iter().find(overlaps) {
                if prior != (hook.address, hook.length) || !loader_ranges.contains(&prior) {
                    return Err(format!(
                        "{} hook at {:#x} overlaps another hook",
                        unit.name, hook.address
                    ));
                }
            } else {
                claimed.push((hook.address, hook.length));
            }
            installed.push(index);
        }
        let edits = PatchEdits::new(
            installed
                .iter()
                .map(|&index| {
                    let hook = &unit.hooks[index];
                    HookEdit {
                        address: hook.address,
                        target: hook.target,
                        length: hook.length,
                    }
                })
                .collect(),
        );
        let applied = apply_patch_with_map(
            image.firmware,
            image.original_length,
            unit.base,
            &unit.payload,
            &edits,
            image.padding,
            image.firmware_base,
            image.slot_end,
        )?;
        if let Some(loader) = loader
            && applied.next_base > loader.module_base
        {
            return Err(format!(
                "{} overlaps the Patch loader's module region",
                unit.name
            ));
        }
        placed.push(Placed {
            base: applied.base,
            payload_len: applied.payload_len,
            installed,
        });
    }

    let identities = loader
        .map(|loader| loader.identity)
        .into_iter()
        .chain(units.iter().map(|unit| unit.identity))
        .collect::<Vec<_>>();
    link_identity_ring(image.firmware, image.firmware_base, &identities)?;
    if let Some(loader) = loader {
        let reserve = loader
            .reserve_end
            .checked_sub(image.firmware_base)
            .ok_or("the loader reserve precedes MAIN")? as usize;
        let grown = reserve
            .checked_sub(image.original_length)
            .ok_or("the loader reserve overlaps stock MAIN")?;
        if grown > image.padding.len() || image.padding[..grown].iter().any(|&b| b != 0xff) {
            return Err("the Patch loader reserve overlaps non-erased package data".into());
        }
        image.firmware.resize(reserve, 0xff);
    }
    Ok(placed)
}

/// Hooks on one site form a chain in unit order: each continuation slot
/// holds the next hook's handler, and the last one holds the loader's
/// handler for the site, if any, and otherwise keeps its stock continuation.
fn chain(units: &mut [Unit], loader: Option<&BTreeMap<u32, u32>>) -> Result<(), String> {
    // The latest hook on each address: (unit, hook).
    let mut tails = BTreeMap::<u32, (usize, usize)>::new();
    for unit in 0..units.len() {
        for index in 0..units[unit].hooks.len() {
            let hook = units[unit].hooks[index].clone();
            if let Some(&(prior_unit, prior_index)) = tails.get(&hook.address) {
                let prior = &units[prior_unit].hooks[prior_index];
                let (Some(prior_continuation), Some(continuation)) =
                    (&prior.continuation, &hook.continuation)
                else {
                    return Err(format!(
                        "{} hook at {:#x} shares a site with a hook that does not chain",
                        units[unit].name, hook.address
                    ));
                };
                if prior.length != hook.length || prior_continuation.site != continuation.site {
                    return Err(format!(
                        "incompatible hooks share {:#x}: {}",
                        hook.address, units[unit].name
                    ));
                }
                let slot = prior_continuation.slot;
                write_slot(&mut units[prior_unit], slot, hook.target | 1)?;
            }
            tails.insert(hook.address, (unit, index));
            if let (Some(continuation), Some(&target)) = (
                &hook.continuation,
                loader.and_then(|targets| targets.get(&hook.address)),
            ) {
                write_slot(&mut units[unit], continuation.slot, target)?;
            }
        }
    }
    Ok(())
}

fn write_slot(unit: &mut Unit, slot: usize, value: u32) -> Result<(), String> {
    unit.payload
        .get_mut(slot..slot + 4)
        .ok_or_else(|| format!("{} continuation slot outside its payload", unit.name))?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u32 = 0x8000_0000;

    fn hook(address: u32, target: u32, slot: Option<usize>) -> UnitHook {
        UnitHook {
            address,
            target,
            length: 4,
            continuation: slot.map(|slot| Continuation {
                site: "midi_in".into(),
                slot,
            }),
        }
    }

    fn unit(name: &str, base: u32, hooks: Vec<UnitHook>) -> Unit {
        Unit {
            name: name.into(),
            base,
            payload: vec![0; 32],
            hooks,
            identity: base + 8,
        }
    }

    fn word(firmware: &[u8], address: u32) -> u32 {
        let offset = (address - BASE) as usize;
        u32::from_le_bytes(firmware[offset..offset + 4].try_into().unwrap())
    }

    #[test]
    fn shared_hooks_chain_in_order_and_end_in_the_loader() {
        let mut firmware = vec![0; 0x80];
        let padding = vec![0xff; 0x200];
        let loader = Loader {
            base: BASE + 0x80,
            payload: vec![0; 32],
            hooks: vec![LoaderHook {
                address: BASE + 0x10,
                target: BASE + 0x90,
                length: 4,
                dispatch: false,
            }],
            identity: BASE + 0x88,
            module_base: BASE + 0x200,
            reserve_end: BASE + 0x220,
        };
        let units = vec![
            unit(
                "first",
                BASE + 0xa0,
                vec![hook(BASE + 0x10, BASE + 0xa0, Some(4))],
            ),
            unit(
                "second",
                BASE + 0xc0,
                vec![hook(BASE + 0x10, BASE + 0xc0, Some(4))],
            ),
        ];
        let image = Image {
            firmware: &mut firmware,
            original_length: 0x80,
            padding: &padding,
            firmware_base: BASE,
            slot_end: BASE + 0x1000,
        };
        let placed = compose(image, Some(&loader), units).unwrap();
        assert_eq!(placed[0].installed, [0]);
        assert!(placed[1].installed.is_empty());
        // The first patch takes the loader's site over.
        assert_eq!(
            &firmware[0x10..0x14],
            &crate::branch(BASE + 0x10, BASE + 0xa0, false)
        );
        assert_eq!(word(&firmware, BASE + 0xa4), BASE + 0xc1);
        assert_eq!(word(&firmware, BASE + 0xc4), BASE + 0x91);
        // The identity ring runs loader, first, second.
        assert_eq!(word(&firmware, BASE + 0x9c), BASE + 0xa8);
        assert_eq!(word(&firmware, BASE + 0xbc), BASE + 0xc8);
        assert_eq!(word(&firmware, BASE + 0xdc), BASE + 0x88);
        assert_eq!(firmware.len(), 0x220);
    }

    #[test]
    fn hooks_that_cannot_chain_are_refused() {
        let padding = vec![0xff; 0x200];
        let compose_units = |units| {
            let mut firmware = vec![0; 0x40];
            let image = Image {
                firmware: &mut firmware,
                original_length: 0x40,
                padding: &padding,
                firmware_base: BASE,
                slot_end: BASE + 0x1000,
            };
            compose(image, None, units)
        };
        let shared = |slot| {
            vec![
                unit(
                    "first",
                    BASE + 0x40,
                    vec![hook(BASE + 0x10, BASE + 0x40, Some(4))],
                ),
                unit(
                    "second",
                    BASE + 0x60,
                    vec![hook(BASE + 0x10, BASE + 0x60, slot)],
                ),
            ]
        };
        assert!(compose_units(shared(Some(4))).is_ok());
        assert!(compose_units(shared(None)).is_err());
        let overlapping = vec![
            unit(
                "first",
                BASE + 0x40,
                vec![hook(BASE + 0x10, BASE + 0x40, None)],
            ),
            unit(
                "second",
                BASE + 0x60,
                vec![hook(BASE + 0x12, BASE + 0x60, None)],
            ),
        ];
        assert!(compose_units(overlapping).is_err());
    }
}
