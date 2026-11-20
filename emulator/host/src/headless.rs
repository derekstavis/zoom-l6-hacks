//! Guest-clock boot and key helpers for hardware integration diagnostics.
use crate::{
    display::PublishedDisplay,
    engine::{Engine, Options},
};
use std::{
    cell::Cell,
    io, thread,
    time::{Duration, Instant},
};
pub struct Headless {
    pub engine: Engine,
    pub display: PublishedDisplay,
    last_frame: Cell<Option<[u8; 1024]>>,
}
impl Headless {
    pub fn start(options: &Options) -> Result<Self, String> {
        let engine = Engine::start(options)?;
        let inputs = engine.inputs.as_ref().ok_or("QEMU required")?;
        let display = PublishedDisplay::new(
            engine.display_memory.as_ref().unwrap().map(),
            inputs.main.clone(),
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            engine,
            display,
            last_frame: Cell::new(None),
        })
    }
    /// Retain the last completed LCD transfer when the guest has no new frame.
    pub fn sample_display(&self) -> io::Result<Option<[u8; 1024]>> {
        if let Some(frame) = self.display.sample()? {
            self.last_frame.set(Some(frame));
        }
        Ok(self.last_frame.get())
    }
    pub fn guest_ms(&self) -> u32 {
        self.engine
            .inputs
            .as_ref()
            .unwrap()
            .panel
            .snapshot()
            .guest_ms
    }
    /// The panel and MAIN guest clocks. The panel clock stops while MAIN
    /// holds the panel CPU, as during an L6 update or first boot.
    fn clocks(&self) -> (u32, u32) {
        let inputs = self.engine.inputs.as_ref().unwrap();
        (inputs.panel.snapshot().guest_ms, inputs.main.snapshot().guest_ms)
    }
    pub fn until(&self, ms: u32) -> io::Result<()> {
        // A loaded host runs the guest slower than real time, so the wall
        // bound is on progress: fail when neither guest clock advances. The
        // caller's own deadline bounds the whole run.
        const STALL: Duration = Duration::from_secs(60);
        let mut progress = self.clocks();
        let mut deadline = Instant::now() + STALL;
        while self.guest_ms() < ms {
            let now = self.clocks();
            if now != progress {
                progress = now;
                deadline = Instant::now() + STALL;
            }
            if self
                .engine
                .inputs
                .as_ref()
                .unwrap()
                .main
                .snapshot()
                .powered_off
            {
                return Err(io::Error::other("guest powered off before deadline"));
            }
            self.sample_display()?;
            if Instant::now() > deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "guest clocks stalled at panel {} ms, MAIN {} ms, before panel {ms} ms",
                        progress.0, progress.1
                    ),
                ));
            }
            thread::sleep(Duration::from_millis(5));
        }
        self.sample_display()?;
        Ok(())
    }
    pub fn wait(&self, ms: u32) -> io::Result<()> {
        self.until(self.guest_ms().saturating_add(ms))
    }
    pub fn tap(&self, key: u32, duration: u32, settle: u32) -> io::Result<()> {
        self.engine.inputs.as_ref().unwrap().tap(key, duration)?;
        self.wait(duration + settle)
    }
    pub fn recorder(&self) -> io::Result<()> {
        self.until(40000)?;
        self.tap(3, 50, 1950)?;
        self.tap(3, 50, 4950)
    }
    pub fn file_transfer(&self) -> io::Result<()> {
        self.recorder()?;
        self.tap(0, 500, 1000)?;
        for _ in 0..5 {
            self.tap(2, 50, 950)?;
        }
        self.tap(3, 50, 2950)
    }
}
