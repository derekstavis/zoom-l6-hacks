use super::*;

#[test]
fn ainr_boundary_pause_rejects_stale_acknowledgment_after_timeout() {
    use core::sync::atomic::AtomicU32;
    let word = AtomicU32::new(0);
    let pause = boundary_pause::BoundaryPause(&word);
    let old = pause.request().unwrap();
    assert!(pause.bypass(1));
    assert!(!pause.acknowledged(old));
    assert!(pause.finish(old));
    let new = pause.request().unwrap();
    assert!(!pause.acknowledge(old));
    assert!(!pause.acknowledged(new));
    assert!(!pause.acknowledged(old));
    assert!(!pause.finish(old));
    assert!(pause.acknowledge(new));
    assert!(pause.acknowledged(new));
    assert!(pause.bypass(1));
    assert!(!pause.bypass(2));
    assert!(!pause.acknowledged(new));
    assert!(!pause.finish(new));
    let following = pause.request().unwrap();
    assert!(pause.bypass(0));
    assert!(pause.acknowledged(following));
    assert!(pause.finish(following));
    assert!(!pause.bypass(0));
    let exhausted = AtomicU32::new(u32::MAX & !3);
    assert!(
        boundary_pause::BoundaryPause(&exhausted)
            .request()
            .is_none()
    );
}

#[test]
fn ainr_boundary_snapshot_packs_and_reconstructs_the_candidate_tail() {
    use boundary_pause::{
        CAPTURE_BYTES, CLONE_BYTES, DESCRIPTOR_OFFSET, SNAPSHOT_END, SNAPSHOT_START,
    };
    let source: Vec<u8> = (0..SNAPSHOT_END)
        .map(|i| (i.wrapping_mul(37) & 255) as u8)
        .collect();
    let mut packed = vec![0xa5; CAPTURE_BYTES + 2];
    unsafe {
        boundary_pause::copy_snapshot(source.as_ptr(), packed.as_mut_ptr().add(1));
    }
    assert_eq!(packed[0], 0xa5);
    assert_eq!(packed[CAPTURE_BYTES + 1], 0xa5);
    assert_eq!(
        &packed[1..=CAPTURE_BYTES],
        &source[SNAPSHOT_START..SNAPSHOT_END]
    );
    assert_eq!(
        &packed[1 + DESCRIPTOR_OFFSET..1 + DESCRIPTOR_OFFSET + 4],
        &source[0xba4c..0xba50]
    );

    let mut clone = vec![0xa5; CLONE_BYTES + 2];
    unsafe {
        boundary_pause::install_snapshot(packed.as_ptr().add(1), clone.as_mut_ptr().add(1));
    }
    assert_eq!(clone[0], 0xa5);
    assert_eq!(clone[CLONE_BYTES + 1], 0xa5);
    let clone = &clone[1..=CLONE_BYTES];
    assert!(clone[..SNAPSHOT_START].iter().all(|&byte| byte == 0));
    assert_eq!(
        &clone[SNAPSHOT_START..SNAPSHOT_END],
        &source[SNAPSHOT_START..SNAPSHOT_END]
    );
    assert!(clone[SNAPSHOT_END..].iter().all(|&byte| byte == 0));
}

#[test]
fn ainr_boundary_cannot_publish_after_pause_invalidation() {
    use core::sync::atomic::AtomicU32;
    let pause_word = AtomicU32::new(0);
    let capture_word = AtomicU32::new(0);
    let pause = boundary_pause::BoundaryPause(&pause_word);
    let capture = handoff_protocol::CaptureProtocol(&capture_word);
    let (unacknowledged_claim, _) = capture.claim().unwrap();
    let unacknowledged = pause.request().unwrap();
    assert!(!pause.publish_capture(unacknowledged, &capture, unacknowledged_claim));
    assert!(!capture.done());
    assert!(pause.finish(unacknowledged));
    let (claim, _) = capture.claim().unwrap();
    let request = pause.request().unwrap();
    assert!(pause.bypass(0));
    // The selector has just performed its last validity check.
    assert!(pause.acknowledged(request));
    // An unexpected callback arrives before publication.
    assert!(!pause.bypass(2));
    assert!(!pause.publish_capture(request, &capture, claim));
    assert!(!capture.done());
    let (retry, _) = capture.claim().unwrap();
    let next = pause.request().unwrap();
    assert!(pause.bypass(0));
    assert!(pause.publish_capture(next, &capture, retry));
    assert!(capture.done());
    assert!(!pause.bypass(0));
    assert!(capture.cancel(false));
    capture.arm();
    let (cancelled, _) = capture.claim().unwrap();
    let third = pause.request().unwrap();
    assert!(pause.bypass(0));
    assert!(capture.cancel(false));
    assert!(!pause.publish_capture(third, &capture, cancelled));
    assert!(!capture.done());
    assert!(!pause.bypass(0));
    capture.arm();
    let (timeout_claim, _) = capture.claim().unwrap();
    let timeout = pause.request().unwrap();
    assert!(pause.bypass(0));
    assert!(pause.finish(timeout));
    assert!(!pause.publish_capture(timeout, &capture, timeout_claim));
    assert!(!capture.done());
}

#[test]
fn refresh_heap_ranges_are_bounded_aligned_and_disjoint() {
    use refresh_handoff::valid_heap_scratch as valid;
    let snapshot = 0x808fa040;
    let len = boundary_pause::CAPTURE_BYTES;
    assert!(valid(snapshot + len, snapshot));
    assert!(valid(snapshot - len, snapshot));
    assert!(!valid(snapshot, snapshot));
    assert!(!valid(snapshot + len - 8, snapshot));
    assert!(!valid(snapshot + len + 1, snapshot));
    assert!(!valid(heap_range::FIRST_PAYLOAD - 16, snapshot));
    assert!(valid(heap_range::END - len, snapshot));
    assert!(!valid(heap_range::END + 8 - len, snapshot));
    assert!(!valid(heap_range::END + 8, snapshot));
    assert!(!valid(snapshot + len, 0));
    assert!(!valid(usize::MAX - 7, snapshot));
    assert!(!valid(snapshot + len, usize::MAX - 7));
}

#[test]
fn snapshot_heap_range_rejects_headers_sentinel_and_overflow() {
    use heap_range::valid_heap_allocation as valid;
    let len = boundary_pause::CAPTURE_BYTES;
    assert!(valid(heap_range::FIRST_PAYLOAD, len));
    assert!(valid(heap_range::END - len, len));
    assert!(!valid(heap_range::FIRST_PAYLOAD - 8, len));
    assert!(!valid(heap_range::FIRST_PAYLOAD + 1, len));
    assert!(!valid(heap_range::END + 8 - len, len));
    assert!(!valid(usize::MAX - 7, len));
    assert!(!valid(heap_range::FIRST_PAYLOAD, 0));
}

#[test]
fn refresh_retries_exclude_stale_writers_and_readers() {
    use core::sync::atomic::AtomicU32;
    let pause = AtomicU32::new(0);
    let capture = AtomicU32::new(0);
    let refresh = refresh_handoff::RefreshHandoff {
        pause: &pause,
        capture: &capture,
    };

    // Cancellation before the audio task claims the request.
    let unclaimed = refresh.request().unwrap();
    refresh.cancel(unclaimed);
    assert!(refresh.claim().is_none());

    // A claimed writer must retire before its scratch is reused.
    let old = refresh.request().unwrap();
    let (boundary, writer) = refresh.claim().unwrap();
    refresh.cancel(old);
    assert!(refresh.claim_completed(old).is_none());
    assert!(
        refresh.request().is_none(),
        "timed-out writer still owns scratch"
    );
    assert!(!refresh.publish(boundary, writer));

    // A late timeout cannot cancel a newer generation.
    let next = refresh.request().unwrap();
    refresh.cancel(old);
    let (boundary, writer) = refresh.claim().unwrap();
    assert!(refresh.publish(boundary, writer));
    assert!(refresh.claim_completed(old).is_none());
    let reader = refresh.claim_completed(next).unwrap();
    assert!(refresh.claim_completed(next).is_none());
    assert!(handoff_protocol::CaptureProtocol(&capture).current_claim(reader));

    // Scratch remains unavailable while an installer holds the reader.
    assert!(refresh.request().is_none(), "reader still owns scratch");
    assert!(!handoff_protocol::CaptureProtocol(&capture).current_claim(reader));
    assert!(!refresh.release(reader));
    let retry = refresh.request().unwrap();
    let (boundary, writer) = refresh.claim().unwrap();
    assert!(refresh.publish(boundary, writer));
    assert!(refresh.claim_completed(next).is_none());
    assert!(refresh.release(refresh.claim_completed(retry).unwrap()));
}

#[test]
fn refresh_timeout_blocks_reuse_then_closed_gate_allows_retry() {
    use core::sync::atomic::AtomicU32;
    use std::sync::Barrier;
    let pause = AtomicU32::new(0);
    let capture = AtomicU32::new(0);
    let refresh = refresh_handoff::RefreshHandoff {
        pause: &pause,
        capture: &capture,
    };
    let request = refresh.request().unwrap();
    let barrier = Barrier::new(2);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let (boundary, writer) = refresh.claim().unwrap();
            barrier.wait();
            barrier.wait();
            assert!(!refresh.publish(boundary, writer));
        });
        barrier.wait();
        refresh.cancel(request);
        assert!(refresh.request().is_none());
        barrier.wait();
        worker.join().unwrap();
    });
    let retry = refresh.request().unwrap();
    let (boundary, writer) = refresh.claim().unwrap();
    assert!(refresh.publish(boundary, writer));
    assert!(refresh.release(refresh.claim_completed(retry).unwrap()));
}

#[test]
fn ainr_capture_cancellation_rejects_stale_claims_across_rearm() {
    use core::sync::atomic::AtomicU32;
    let word = AtomicU32::new(0);
    let protocol = handoff_protocol::CaptureProtocol(&word);
    let (old_claim, step) = protocol.claim().unwrap();
    assert_eq!(step, 0);
    assert!(protocol.cancel(true));
    assert!(!protocol.finish(old_claim, 1, false));
    protocol.arm();
    let (new_claim, step) = protocol.claim().unwrap();
    assert_eq!(step, 0);
    assert!(!protocol.finish(old_claim, 15, true));
    assert!(!protocol.done());
    assert!(protocol.finish(new_claim, 1, false));
    let (claim, step) = protocol.claim().unwrap();
    assert_eq!(step, 1);
    assert!(protocol.finish(claim, 15, true));
    assert!(protocol.done());
    assert!(!protocol.cancel(true));
    assert!(protocol.done());
    assert!(protocol.cancel(false));
    assert!(!protocol.done());
    protocol.arm();
    assert_eq!(protocol.claim().unwrap().1, 0);
}

#[test]
fn ainr_reentry_invalidates_completed_snapshot_and_waits_for_old_writer() {
    use core::sync::atomic::AtomicU32;
    let word = AtomicU32::new(0);
    let protocol = handoff_protocol::CaptureProtocol(&word);
    let (first, _) = protocol.claim().unwrap();
    assert!(protocol.finish(first, 15, true));
    let old_ready = (first & !0x3f) | (15 << 2) | 2;
    assert!(protocol.current_done(old_ready));

    protocol.restart();
    assert!(!protocol.current_done(old_ready));
    let (second, step) = protocol.claim().unwrap();
    assert_eq!(step, 0);
    protocol.restart();
    assert!(protocol.claim().is_none(), "old writer still owns snapshot");
    assert!(!protocol.finish(second, 15, true));
    let (third, step) = protocol.claim().unwrap();
    assert_eq!(step, 0);
    assert!(protocol.finish(third, 15, true));
    assert!(!protocol.current_done(old_ready));
}

#[test]
fn ainr_capture_epoch_exhaustion_fails_closed() {
    use core::sync::atomic::AtomicU32;
    let word = AtomicU32::new(0xffff_ff80);
    let protocol = handoff_protocol::CaptureProtocol(&word);
    let (claim, _) = protocol.claim().unwrap();
    assert!(protocol.cancel(true));
    assert!(!protocol.finish(claim, 1, false));
    protocol.arm();
    assert!(protocol.claim().is_none());
    assert!(!protocol.done());
}

#[test]
fn ainr_capture_waits_for_cancelled_writer_before_reusing_snapshot() {
    use core::sync::atomic::AtomicU32;
    let word = AtomicU32::new(0);
    let protocol = handoff_protocol::CaptureProtocol(&word);
    let (old, _) = protocol.claim().unwrap();
    assert!(protocol.cancel(true));
    protocol.arm();
    assert!(protocol.claim().is_none());
    assert!(!protocol.finish(old, 1, false));
    let (new, step) = protocol.claim().unwrap();
    assert_eq!(step, 0);
    assert!(protocol.cancel(true));
    protocol.arm();
    // A second cancellation revokes the pending rearm request.
    assert!(protocol.cancel(true));
    assert!(!protocol.finish(old, 1, false));
    assert!(protocol.claim().is_none());
    assert!(!protocol.finish(new, 1, false));
    assert!(protocol.claim().is_none());
    protocol.arm();
    assert_eq!(protocol.claim().unwrap().1, 0);
}

#[test]
fn ainr_install_holds_snapshot_and_rejects_cancelled_generation() {
    use core::sync::atomic::AtomicU32;
    let word = AtomicU32::new(0);
    let protocol = handoff_protocol::CaptureProtocol(&word);
    let (capture, _) = protocol.claim().unwrap();
    assert!(protocol.finish(capture, 15, true));
    let install = protocol.claim_install().unwrap();
    assert!(protocol.claim_install().is_none());
    assert!(protocol.cancel(true));
    protocol.arm();
    assert!(protocol.claim().is_none());
    assert!(!protocol.finish(install, 15, true));
    assert!(!protocol.current_done((install & !3) | 2));
    let (capture, _) = protocol.claim().unwrap();
    assert!(protocol.finish(capture, 15, true));
    let install = protocol.claim_install().unwrap();
    assert!(protocol.finish(install, 15, true));
    let ready = (install & !3) | 2;
    assert!(protocol.current_done(ready));
    assert!(protocol.cancel(false));
    protocol.arm();
    let (capture, _) = protocol.claim().unwrap();
    assert!(protocol.finish(capture, 15, true));
    assert!(!protocol.current_done(ready));
}

#[test]
fn control_queue_preserves_fifo_at_capacity_and_counter_wrap() {
    use core::sync::atomic::{AtomicU32, Ordering};
    use event_queue::{CAPACITY, Queue, WORDS_WITH_GENERATIONS};
    let words: [AtomicU32; WORDS_WITH_GENERATIONS] =
        [const { AtomicU32::new(0) }; WORDS_WITH_GENERATIONS];
    let start = u32::MAX - 31;
    words[0].store(start, Ordering::Relaxed);
    words[1].store(start, Ordering::Relaxed);
    let queue = Queue::new(&words, true);
    for n in 0..CAPACITY {
        assert_eq!(queue.push(n, || n ^ 0x55aa), Ok(start.wrapping_add(n + 1)));
    }
    assert_eq!(
        queue.push(999, || panic!("full queue requested generation")),
        Err(1)
    );
    let (head, mut tail) = queue.batch().unwrap();
    for n in 0..CAPACITY {
        assert_ne!(tail, head);
        assert_eq!(queue.read(tail), (n, n ^ 0x55aa));
        tail = tail.wrapping_add(1);
        queue.commit(tail);
    }
    assert_eq!(tail, head);
    assert_eq!(queue.push(123, || 456), Ok(head.wrapping_add(1)));
    assert_eq!(queue.read(tail), (123, 456));
}

#[test]
fn standalone_queue_does_not_access_generation_storage() {
    use core::sync::atomic::AtomicU32;
    use event_queue::{CAPACITY, HEADER_WORDS, Queue};
    let words: [AtomicU32; HEADER_WORDS + CAPACITY as usize] =
        [const { AtomicU32::new(0) }; HEADER_WORDS + CAPACITY as usize];
    let queue = Queue::new(&words, false);
    assert_eq!(
        queue.push(77, || panic!("standalone requested a generation")),
        Ok(1)
    );
    let (head, tail) = queue.batch().unwrap();
    assert_eq!((head, tail), (1, 0));
    assert_eq!(queue.push(88, || 1), Ok(2));
    assert_eq!(queue.read(tail), (77, 0));
    queue.commit(tail + 1);
    assert_eq!(head, 1, "the original batch excludes the later event");
    assert_eq!(queue.batch(), Some((2, 1)));
    assert_eq!(queue.read(1), (88, 0));
}

#[test]
fn control_queue_rejects_impossible_indices_without_consuming() {
    use core::sync::atomic::{AtomicU32, Ordering};
    use event_queue::{CAPACITY, Queue, WORDS_WITH_GENERATIONS};
    let words: [AtomicU32; WORDS_WITH_GENERATIONS] =
        [const { AtomicU32::new(0) }; WORDS_WITH_GENERATIONS];
    let queue = Queue::new(&words, true);
    for (head, tail) in [(65, 0), (0, 1), (u32::MAX, 0), (32, u32::MAX - 32)] {
        words[0].store(head, Ordering::Relaxed);
        words[1].store(tail, Ordering::Relaxed);
        assert_eq!(queue.batch(), None, "head {head}, tail {tail}");
        assert_eq!(words[1].load(Ordering::Relaxed), tail);
    }
    words[0].store(32, Ordering::Relaxed);
    words[1].store(32u32.wrapping_sub(CAPACITY), Ordering::Relaxed);
    assert_eq!(queue.batch(), Some((32, 32u32.wrapping_sub(CAPACITY))));
}

#[test]
fn control_queue_publishes_events_with_their_generations_between_threads() {
    use core::sync::atomic::AtomicU32;
    use event_queue::{Queue, WORDS_WITH_GENERATIONS};
    let words: [AtomicU32; WORDS_WITH_GENERATIONS] =
        [const { AtomicU32::new(0) }; WORDS_WITH_GENERATIONS];
    let queue = Queue::new(&words, true);
    let mut mismatch = None;
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for n in 0..100_000u32 {
                while queue.push(n, || !n).is_err() {
                    std::thread::yield_now();
                }
            }
        });
        let mut expected = 0u32;
        while expected < 100_000 {
            let (head, mut tail) = queue.batch().unwrap();
            while tail != head {
                let observed = queue.read(tail);
                if observed != (expected, !expected) && mismatch.is_none() {
                    mismatch = Some((expected, observed));
                }
                expected += 1;
                tail = tail.wrapping_add(1);
                queue.commit(tail);
            }
            std::thread::yield_now();
        }
    });
    assert_eq!(mismatch, None);
}
