//! Firmware lifecycle: factory fixtures, updates and the browser patch builder.
use super::*;

pub(super) fn prepare_factory_fixture(options: &mut Options) -> Result<(), String> {
    let state = options.logs.join("factory-state");
    let card = options.logs.join("factory-card.img");
    if options.state_dir.as_ref() == Some(&state)
        && options.sd_image.as_ref() == Some(&card)
        && state.is_dir()
        && card.is_file()
    {
        return Ok(());
    }
    if options.state_dir.is_some() || options.sd_image.is_some() {
        return Err("factory-protocol owns its disposable state and SD fixture".into());
    }
    let main = fs::read(options.firmware_dir.join("main_firmware.bin"))
        .map_err(|error| error.to_string())?;
    if !crate::firmware::is_stock(&main, l6fw::Model::L6max) {
        return Err("factory-protocol requires the hash-verified L6max MAIN image".into());
    }
    fs::create_dir_all(&options.logs).map_err(|error| error.to_string())?;
    fs::create_dir(&state).map_err(|error| error.to_string())?;
    create_factory_card(&card)?;
    options.sd_image = Some(card);
    options.state_dir = Some(state);
    options.no_sd = false;
    Ok(())
}

pub(super) fn create_factory_card(card: &std::path::Path) -> Result<(), String> {
    let mut volume = sd_image::create_empty(card, "L6MAX").map_err(|e| e.to_string())?;
    volume
        .write(
            "D445_JIG_000.txt",
            b"emulator fixture; no calibration parameters\n",
        )
        .and_then(|()| volume.flush())
        .map_err(|error| error.to_string())
}
