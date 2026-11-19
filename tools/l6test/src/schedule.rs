//! The bounded job scheduler.
use std::thread;

/// Run jobs on up to `limit` workers. An exclusive job starts only when no
/// other job is running and holds back later jobs until it finishes. Results
/// keep job order; `progress` receives the completed results so far.
pub(crate) fn schedule<J: Sync, R: Send>(
    jobs: &[J],
    limit: usize,
    exclusive: impl Fn(&J) -> bool + Sync,
    run: impl Fn(&J) -> R + Sync,
    progress: impl Fn(&[&R]) -> Result<(), String> + Sync,
) -> Result<Vec<R>, String> {
    use std::sync::{Condvar, Mutex};
    struct State<R> {
        next: usize,
        running: usize,
        exclusive: bool,
        results: Vec<Option<R>>,
        error: Option<String>,
    }
    let state = Mutex::new(State {
        next: 0,
        running: 0,
        exclusive: false,
        results: jobs.iter().map(|_| None).collect(),
        error: None,
    });
    let changed = Condvar::new();
    thread::scope(|scope| {
        for _ in 0..limit.clamp(1, jobs.len().max(1)) {
            scope.spawn(|| {
                loop {
                    let mut guard = state.lock().unwrap();
                    let index = loop {
                        if guard.next == jobs.len() || guard.error.is_some() {
                            return;
                        }
                        let exclusive = exclusive(&jobs[guard.next]);
                        if !guard.exclusive && (guard.running == 0 || !exclusive) {
                            break guard.next;
                        }
                        guard = changed.wait(guard).unwrap();
                    };
                    let exclusive = exclusive(&jobs[index]);
                    guard.next += 1;
                    guard.running += 1;
                    guard.exclusive = exclusive;
                    drop(guard);
                    let result = run(&jobs[index]);
                    let mut guard = state.lock().unwrap();
                    guard.results[index] = Some(result);
                    guard.running -= 1;
                    guard.exclusive &= !exclusive;
                    let finished: Vec<&R> = guard.results.iter().flatten().collect();
                    if let Err(error) = progress(&finished) {
                        guard.error = Some(error);
                    }
                    drop(guard);
                    changed.notify_all();
                }
            });
        }
    });
    let state = state.into_inner().unwrap();
    if let Some(error) = state.error {
        return Err(error);
    }
    Ok(state.results.into_iter().flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn scheduler_bounds_concurrency_and_isolates_exclusive_jobs() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let exclusive_overlap = AtomicUsize::new(0);
        let jobs: Vec<(usize, bool)> = (0..12).map(|i| (i, i % 5 == 4)).collect();
        let results = schedule(
            &jobs,
            3,
            |job| job.1,
            |job| {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                if job.1 && now != 1 {
                    exclusive_overlap.fetch_add(1, Ordering::SeqCst);
                }
                thread::sleep(Duration::from_millis(20));
                if job.1 && running.load(Ordering::SeqCst) != 1 {
                    exclusive_overlap.fetch_add(1, Ordering::SeqCst);
                }
                running.fetch_sub(1, Ordering::SeqCst);
                job.0
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(results, (0..12).collect::<Vec<_>>());
        assert_eq!(peak.load(Ordering::SeqCst), 3);
        assert_eq!(exclusive_overlap.load(Ordering::SeqCst), 0);
    }
}
