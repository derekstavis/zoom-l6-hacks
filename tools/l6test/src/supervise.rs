//! Supervised child process groups and run cancellation.
use std::{
    fs,
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(crate) static CANCELLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
pub(crate) extern "C" fn interrupt(_: libc::c_int) {
    CANCELLED.store(true, std::sync::atomic::Ordering::Relaxed);
}
pub(crate) fn cancelled() -> bool {
    CANCELLED.load(std::sync::atomic::Ordering::Relaxed)
}
pub(crate) struct ChildGroup(Child, bool);
impl Drop for ChildGroup {
    fn drop(&mut self) {
        // The driver creates this group. Always retire descendants, even after
        // an unexpectedly successful parent exits without reaping QEMU.
        if self.1 {
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGTERM);
            }
        } else {
            let _ = self.0.kill();
        }
        if matches!(self.0.try_wait(), Ok(None)) {
            let end = Instant::now() + Duration::from_secs(3);
            while Instant::now() < end && matches!(self.0.try_wait(), Ok(None)) {
                thread::sleep(Duration::from_millis(20));
            }
        }
        if self.1 {
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
        }
        let _ = self.0.wait();
    }
}
pub(crate) fn supervise(mut command: Command, dir: &Path, timeout: u64) -> Result<bool, String> {
    let stdout = fs::File::create(dir.join("stdout.log")).map_err(|e| e.to_string())?;
    let stderr = fs::File::create(dir.join("stderr.log")).map_err(|e| e.to_string())?;
    command.stdin(Stdio::null()).stdout(stdout).stderr(stderr);
    let own_group = std::env::var_os("L6TEST_CAMPAIGN_MEMBER").is_none();
    if own_group {
        command.process_group(0);
    }
    let mut child = ChildGroup(command.spawn().map_err(|e| e.to_string())?, own_group);
    let started = Instant::now();
    let mut next = Instant::now() + Duration::from_secs(15);
    loop {
        if cancelled() {
            return Err("run interrupted; child process group stopped".into());
        }
        if let Some(status) = child.0.try_wait().map_err(|e| e.to_string())? {
            return Ok(status.success());
        }
        if started.elapsed() > Duration::from_secs(timeout) {
            return Err("child exceeded wall-clock limit; its process group was stopped".into());
        }
        if Instant::now() >= next {
            tracing::info!(event="child_progress",wall_seconds=started.elapsed().as_secs_f64(),artifacts=%dir.display(),"child running");
            next = Instant::now() + Duration::from_secs(15);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };
    static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn temp() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "l6test-unit-{}-{stamp}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn campaign_preparation_timeout_stops_nested_guest_group() {
        if let Some(marker) = env::var_os("L6TEST_FAULT_MARKER") {
            let dir = std::path::PathBuf::from(&marker)
                .parent()
                .unwrap()
                .join("inner");
            fs::create_dir_all(&dir).unwrap();
            let mut guest = Command::new("sh");
            guest
                .args(["-c", "(sleep 3; printf leaked > \"$1\") & wait", "sh"])
                .arg(marker);
            let _ = supervise(guest, &dir, 30);
            return;
        }
        let dir = temp();
        let marker = dir.join("leaked");
        let mut command = Command::new(env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "supervise::tests::campaign_preparation_timeout_stops_nested_guest_group",
                "--nocapture",
            ])
            .env("L6TEST_CAMPAIGN_MEMBER", "1")
            .env("L6TEST_FAULT_MARKER", &marker);
        assert!(
            supervise(command, &dir, 1)
                .unwrap_err()
                .contains("wall-clock")
        );
        thread::sleep(Duration::from_millis(3200));
        assert!(!marker.exists(), "nested guest survived campaign timeout");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn timeout_stops_descendants_as_well_as_the_parent() {
        let dir = temp();
        let marker = dir.join("leaked");
        let mut command = Command::new("sh");
        command
            .args(["-c", "(sleep 2; printf leaked > \"$1\") & wait", "sh"])
            .arg(&marker);
        assert!(
            supervise(command, &dir, 1)
                .unwrap_err()
                .contains("wall-clock")
        );
        thread::sleep(Duration::from_millis(1600));
        assert!(!marker.exists(), "descendant escaped process-group cleanup");
        fs::remove_dir_all(dir).unwrap();
    }
}
