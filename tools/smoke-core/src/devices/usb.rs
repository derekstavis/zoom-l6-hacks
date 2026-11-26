//! L6 USB startup.
use super::*;

/// Attach the cable and wait, under the step's deadlines, until firmware runs
/// its device controller. Firmware starts it after boot (about 22 guest s on
/// L6; about 4 s on L6max, or 42 s with the cable attached at power-on), and
/// the device model refuses tokens with ENODEV until then.
pub(super) fn wait_usb_controller(
    session: &Session,
    usb: &l6max_host::usb::UsbHost,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<(), String> {
    usb.connect(true).map_err(|error| error.to_string())?;
    let start = guest_now(session);
    let ceiling = Instant::now() + Duration::from_secs(wall_timeout_s);
    while !usb
        .controller_running()
        .map_err(|error| error.to_string())?
    {
        if Instant::now() >= ceiling || guest_now(session).wrapping_sub(start) > guest_timeout_ms {
            return Err("USB device controller did not start".into());
        }
        session.poll()?;
        thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}
