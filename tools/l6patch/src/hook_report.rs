//! Typed inspection of hook metadata emitted by firmware and SD patch builds.
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirmwareHook {
    pub address: u32,
    pub target: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DispatchSite {
    pub address: u32,
    pub target: u32,
    pub index: u32,
}

fn checked_report(report: &[u8], image: &[u8], model: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_slice(report).map_err(|e| e.to_string())?;
    if value["model"].as_str() != Some(model)
        || value["image_length"].as_u64() != Some(image.len() as u64)
        || value["patched_sha256"].as_str() != Some(format!("{:x}", Sha256::digest(image)).as_str())
    {
        return Err("patch report does not identify the supplied model and MAIN image".into());
    }
    Ok(value)
}
fn number(value: &Value, field: &str) -> Result<u32, String> {
    value[field]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| format!("invalid {field} in patch report"))
}
/// Locate a unique browser-style firmware hook and verify the report identifies MAIN.
pub fn firmware_hook(
    report: &[u8],
    image: &[u8],
    model: &str,
    symbol: &str,
    address: u32,
) -> Result<FirmwareHook, String> {
    let report = checked_report(report, image, model)?;
    let hooks = report["hooks"]
        .as_array()
        .ok_or("patch report has no firmware hooks")?;
    let mut found = hooks
        .iter()
        .filter(|h| h["symbol"].as_str() == Some(symbol));
    let hook = found.next().ok_or("patch report has no requested hook")?;
    if found.next().is_some() || number(hook, "address")? != address {
        return Err("ambiguous or unexpected firmware hook".into());
    }
    Ok(FirmwareHook {
        address,
        target: number(hook, "target")? & !1,
    })
}
/// Locate the selected dynamic dispatch site in a loader report identifying MAIN.
pub fn dispatch_site(
    report: &[u8],
    image: &[u8],
    model: &str,
    address: u32,
) -> Result<DispatchSite, String> {
    let report = checked_report(report, image, model)?;
    let patches = report["patches"]
        .as_array()
        .ok_or("loader report has no patches")?;
    let mut dispatch = patches
        .iter()
        .filter(|p| p["name"].as_str() == Some("dynamic-dispatch"));
    let sites = dispatch
        .next()
        .and_then(|p| p["sites"].as_array())
        .ok_or("loader report has no dispatch sites")?;
    if dispatch.next().is_some() {
        return Err("ambiguous dynamic dispatch report".into());
    }
    let mut matches = sites
        .iter()
        .enumerate()
        .filter(|(_, s)| s["address"].as_u64() == Some(u64::from(address)));
    let (index, site) = matches.next().ok_or("dispatch site missing")?;
    if matches.next().is_some() {
        return Err("ambiguous dispatch site".into());
    }
    Ok(DispatchSite {
        address,
        target: number(site, "target")? & !1,
        index: u32::try_from(index).map_err(|e| e.to_string())?,
    })
}
/// Decode an SD module and return the unique entry offset for its hook site.
pub fn module_hook_offset(module: &[u8], address: u32) -> Result<u32, String> {
    let module = patch_abi::dynamic::Module::decode(module)
        .map_err(|e| format!("invalid SD patch module: {e:?}"))?;
    let mut hooks = module.hooks.iter().filter(|h| h.address == address);
    let hook = hooks.next().ok_or("module has no requested hook")?;
    if hooks.next().is_some() || hook.entry_offset as usize >= module.memory_len {
        return Err("ambiguous or invalid SD module hook".into());
    }
    Ok(hook.entry_offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reports_require_matching_image_and_unambiguous_hook() {
        let image = b"test image";
        let digest = format!("{:x}", Sha256::digest(image));
        let report = format!(
            r#"{{"model":"L6","image_length":{},"patched_sha256":"{}","hooks":[{{"symbol":"midi_in_hook","address":100,"target":201}}]}}"#,
            image.len(),
            digest
        );
        assert_eq!(
            firmware_hook(report.as_bytes(), image, "L6", "midi_in_hook", 100)
                .unwrap()
                .target,
            200
        );
        assert!(firmware_hook(report.as_bytes(), b"changed", "L6", "midi_in_hook", 100).is_err());
        assert!(firmware_hook(report.as_bytes(), image, "L6max", "midi_in_hook", 100).is_err());
        assert!(firmware_hook(report.as_bytes(), image, "L6", "midi_in_hook", 102).is_err());
    }
    #[test]
    fn loader_sites_require_unique_matching_address() {
        let image = b"loader";
        let report = format!(
            r#"{{"model":"L6","image_length":{},"patched_sha256":"{:x}","patches":[{{"name":"dynamic-dispatch","sites":[{{"address":99,"target":200}},{{"address":100,"target":300}}]}}]}}"#,
            image.len(),
            Sha256::digest(image)
        );
        assert_eq!(
            dispatch_site(report.as_bytes(), image, "L6", 100).unwrap(),
            DispatchSite {
                address: 100,
                target: 300,
                index: 1
            }
        );
        assert!(dispatch_site(report.as_bytes(), image, "L6", 101).is_err());
        assert!(module_hook_offset(b"invalid", 100).is_err());
    }
}
