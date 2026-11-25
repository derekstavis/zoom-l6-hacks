use super::*;
static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn sample(guest_ms: u32) -> Observation {
    Observation {
        data: PanelData {
            powered: true,
            ..Default::default()
        },
        main: InputStatus {
            connected: true,
            ..Default::default()
        },
        panel: InputStatus {
            connected: true,
            guest_ms,
            ..Default::default()
        },
        reports: 0,
        traces: BTreeMap::new(),
    }
}
fn test_runner(steps: &str) -> Runner {
    let scenario =
        Scenario::parse(&format!("version=1\nname='unit'\nmodels=['l6']\n{steps}")).unwrap();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    Runner::new(
        scenario,
        Model::L6,
        std::env::temp_dir().join(format!(
            "smoke-unit-{}-{stamp}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )),
    )
    .unwrap()
}

#[test]
fn display_reference_settling_requires_a_continuous_matching_guest_interval() {
    let text = "version=1\nname='display-settle'\nmodels=['l6max']\n[[step]]\naction='expect-display-image'\nimage='tests/emulator/references/l6max-recorder-home.png'\nsettle_ms=500\n";
    let scenario = Scenario::parse(text).unwrap();
    let mut runner = Runner::new(
        scenario,
        Model::L6max,
        std::env::temp_dir().join(format!(
            "smoke-display-settle-{}",
            NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )),
    )
    .unwrap();
    let reference = image::open(
        crate::scenario::spec_path(std::path::Path::new(
            "tests/emulator/references/l6max-recorder-home.png",
        ))
        .unwrap(),
    )
    .unwrap()
    .to_luma8();
    let mut matched = sample(0);
    for y in 0..64 {
        for x in 0..128 {
            if reference.get_pixel(x, y).0[0] != 0 {
                let index = ((63 - y as usize) / 8) * 128 + x as usize;
                matched.data.display[index] |= 1 << ((63 - y as usize) % 8);
            }
        }
    }
    let now = Instant::now();
    assert!(matches!(
        runner.advance(matched.clone(), now).unwrap(),
        Progress::Waiting
    ));
    matched.panel.guest_ms = 300;
    matched.data.display[0] ^= 1;
    assert!(matches!(
        runner.advance(matched.clone(), now).unwrap(),
        Progress::Waiting
    ));
    matched.data.display[0] ^= 1;
    matched.panel.guest_ms = 400;
    assert!(matches!(
        runner.advance(matched.clone(), now).unwrap(),
        Progress::Waiting
    ));
    matched.panel.guest_ms = 899;
    assert!(matches!(
        runner.advance(matched.clone(), now).unwrap(),
        Progress::Waiting
    ));
    matched.panel.guest_ms = 900;
    assert!(matches!(
        runner.advance(matched, now).unwrap(),
        Progress::Complete
    ));
    assert!(Scenario::parse(&text.replace("settle_ms=500", "settle_ms=11000")).is_err());
}

#[test]
fn direct_usb_midi_cable_is_configurable_and_range_checked() {
    let default = Scenario::parse(
        "version=1\nname='midi'\nmodels=['l6max']\nbackends=['headless']\n[[step]]\naction='direct-usb-midi-round-trip'\n",
    )
    .unwrap();
    assert!(matches!(
        default.step[0].action,
        Action::DirectUsbMidiRoundTrip {
            cable: 1,
            expect_ring_change: true
        }
    ));

    let ignored_cable = Scenario::parse(
        "version=1\nname='midi'\nmodels=['l6max']\nbackends=['headless']\n[[step]]\naction='direct-usb-midi-round-trip'\ncable=5\nexpect_ring_change=false\n",
    )
    .unwrap();
    assert!(matches!(
        ignored_cable.step[0].action,
        Action::DirectUsbMidiRoundTrip {
            cable: 5,
            expect_ring_change: false
        }
    ));
    assert!(Scenario::parse(
        "version=1\nname='midi'\nmodels=['l6max']\nbackends=['headless']\n[[step]]\naction='direct-usb-midi-round-trip'\ncable=16\n",
    )
    .is_err());
}
#[test]
fn display_after_press_requires_ack_and_new_display() {
    let scenario = Scenario::parse("version=1\nname='press-screen'\nmodels=['l6max']\n[[step]]\naction='press'\ncontrol='recorder.down'\n[[step]]\naction='expect-display-after-press'\n").unwrap();
    let logs = std::env::temp_dir().join(format!(
        "smoke-press-screen-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut runner = Runner::new(scenario, Model::L6max, logs.clone()).unwrap();
    let now = Instant::now();
    assert!(matches!(
        runner.advance(sample(0), now).unwrap(),
        Progress::Dispatch(_)
    ));
    let mut obs = sample(50);
    obs.data.display[0] = 1;
    assert!(matches!(
        runner.advance(obs.clone(), now).unwrap(),
        Progress::Waiting
    ));
    obs.data.display[0] = 0;
    obs.main.sent = 2;
    obs.main.applied = 2;
    obs.panel.sent = 2;
    obs.panel.applied = 2;
    assert!(matches!(
        runner.advance(obs.clone(), now).unwrap(),
        Progress::Waiting
    ));
    obs.data.display[0] = 1;
    assert!(matches!(
        runner.advance(obs, now).unwrap(),
        Progress::Complete
    ));
    fs::remove_dir_all(logs).unwrap();
    assert!(Scenario::parse("version=1\nname='invalid'\nmodels=['l6']\n[[step]]\naction='expect-display-after-press'\n").is_err());
}

#[test]
fn display_after_press_accepts_the_changed_frame_when_ack_arrives() {
    let scenario = Scenario::parse("version=1\nname='ack-frame'\nmodels=['l6max']\n[[step]]\naction='press'\ncontrol='recorder.down'\n[[step]]\naction='expect-display-after-press'\n").unwrap();
    let logs = std::env::temp_dir().join(format!(
        "smoke-press-ack-frame-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut runner = Runner::new(scenario, Model::L6max, logs.clone()).unwrap();
    let now = Instant::now();
    assert!(matches!(
        runner.advance(sample(0), now).unwrap(),
        Progress::Dispatch(_)
    ));
    let mut changed = sample(20);
    changed.data.display[0] = 1;
    assert!(matches!(
        runner.advance(changed.clone(), now).unwrap(),
        Progress::Waiting
    ));
    changed.main.sent = 2;
    changed.main.applied = 2;
    changed.panel.sent = 2;
    changed.panel.applied = 2;
    assert!(matches!(
        runner.advance(changed, now).unwrap(),
        Progress::Complete
    ));
    fs::remove_dir_all(logs).unwrap();
}

#[test]
fn display_after_press_keeps_the_press_guest_and_wall_deadlines() {
    for wall in [false, true] {
        let scenario = Scenario::parse("version=1\nname='deadline'\nmodels=['l6max']\n[[step]]\naction='press'\ncontrol='recorder.down'\nguest_timeout_ms=100\nwall_timeout_s=1\n[[step]]\naction='expect-display-after-press'\nguest_timeout_ms=1000\nwall_timeout_s=10\n").unwrap();
        let logs = std::env::temp_dir().join(format!(
            "smoke-press-deadline-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let mut runner = Runner::new(scenario, Model::L6max, logs.clone()).unwrap();
        let now = Instant::now();
        assert!(matches!(
            runner.advance(sample(0), now).unwrap(),
            Progress::Dispatch(_)
        ));
        let mut ack = sample(50);
        ack.main.sent = 2;
        ack.main.applied = 2;
        ack.panel.sent = 2;
        ack.panel.applied = 2;
        assert!(matches!(
            runner
                .advance(ack.clone(), now + Duration::from_millis(500))
                .unwrap(),
            Progress::Waiting
        ));
        ack.data.display[0] = 1;
        if wall {
            assert!(
                runner
                    .advance(ack, now + Duration::from_millis(1001))
                    .unwrap_err()
                    .contains("press deadline")
            );
        } else {
            ack.panel.guest_ms = 101;
            assert!(
                runner
                    .advance(ack, now + Duration::from_millis(501))
                    .unwrap_err()
                    .contains("press deadline")
            );
        }
        fs::remove_dir_all(logs).unwrap();
    }
}

#[test]
fn display_after_press_does_not_start_a_shorter_new_deadline() {
    let scenario = Scenario::parse("version=1\nname='long-press-budget'\nmodels=['l6max']\n[[step]]\naction='press'\ncontrol='recorder.down'\nguest_timeout_ms=2000\nwall_timeout_s=10\n[[step]]\naction='expect-display-after-press'\nguest_timeout_ms=100\nwall_timeout_s=1\n").unwrap();
    let logs = std::env::temp_dir().join(format!(
        "smoke-press-long-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut runner = Runner::new(scenario, Model::L6max, logs.clone()).unwrap();
    let now = Instant::now();
    runner.advance(sample(0), now).unwrap();
    let mut ack = sample(50);
    ack.main.sent = 2;
    ack.main.applied = 2;
    ack.panel.sent = 2;
    ack.panel.applied = 2;
    assert!(matches!(
        runner.advance(ack.clone(), now).unwrap(),
        Progress::Waiting
    ));
    ack.panel.guest_ms = 500;
    assert!(matches!(
        runner
            .advance(ack.clone(), now + Duration::from_secs(2))
            .unwrap(),
        Progress::Waiting
    ));
    ack.data.display[0] = 1;
    assert!(matches!(
        runner.advance(ack, now + Duration::from_secs(3)).unwrap(),
        Progress::Complete
    ));
    fs::remove_dir_all(logs).unwrap();
}

#[test]
fn display_after_press_requires_adjacent_press_for_each_model() {
    let prefix = "version=1\nname='adjacent'\nmodels=['l6max']\n";
    assert!(
        Scenario::parse(&format!(
            "{prefix}[[step]]\naction='expect-display-after-press'\n"
        ))
        .is_err()
    );
    assert!(Scenario::parse(&format!("{prefix}[[step]]\naction='press'\ncontrol='recorder.down'\n[[step]]\naction='delay'\nguest_ms=1\n[[step]]\naction='expect-display-after-press'\n")).is_err());
    assert!(Scenario::parse(&format!("{prefix}[[step]]\naction='press'\ncontrol='recorder.down'\n[[step]]\naction='expect-display-after-press'\n")).is_ok());
}

#[test]
fn display_after_press_rejects_new_input_and_restart_invalidates_baseline() {
    let make_runner = || {
        let scenario = Scenario::parse("version=1\nname='baseline'\nmodels=['l6max']\n[[step]]\naction='press'\ncontrol='recorder.down'\n[[step]]\naction='expect-display-after-press'\n").unwrap();
        Runner::new(
            scenario,
            Model::L6max,
            std::env::temp_dir().join(format!(
                "smoke-press-baseline-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            )),
        )
        .unwrap()
    };
    let now = Instant::now();
    let mut runner = make_runner();
    runner.advance(sample(0), now).unwrap();
    let mut ack = sample(1);
    ack.panel.sent = 2;
    ack.panel.applied = 2;
    ack.main.sent = 2;
    ack.main.applied = 2;
    assert!(matches!(
        runner.advance(ack.clone(), now).unwrap(),
        Progress::Waiting
    ));
    ack.panel.sent = 3;
    ack.main.sent = 3;
    assert!(
        runner
            .advance(ack.clone(), now)
            .unwrap_err()
            .contains("unexpected concurrent input")
    );
    let logs = runner.logs.clone();
    runner.guest_restarted(&logs);
    assert!(
        runner
            .advance(ack, now)
            .unwrap_err()
            .contains("preceding press")
    );
    fs::remove_dir_all(logs).unwrap();
}

#[test]
fn output_alone_cannot_pass_before_the_command_acknowledgement() {
    let mut runner = test_runner(
        "[[step]]\naction='encoder'\nchannel=1\nsteps=12\n[[step]]\naction='expect-ring'\nchannel=1\nmask=31\nchanged=true\nonly=true\n",
    );
    let now = Instant::now();
    assert!(matches!(
        runner.advance(sample(0), now).unwrap(),
        Progress::Dispatch(_)
    ));
    let mut feedback = sample(100);
    for &(row, column) in l6max_host::indicators::RING_PINS[0].iter().take(5) {
        feedback.data.indicators[row] |= 1 << (column ^ 7);
    }
    feedback.reports = 12;
    feedback.panel.sent = 1;
    assert!(matches!(
        runner.advance(feedback.clone(), now).unwrap(),
        Progress::Waiting
    ));
    feedback.panel.applied = 1;
    assert!(matches!(
        runner.advance(feedback.clone(), now).unwrap(),
        Progress::Waiting
    ));
    feedback.panel.guest_ms = 1100;
    assert!(matches!(
        runner.advance(feedback, now).unwrap(),
        Progress::Complete
    ));
    runner.finish("PASS", None).unwrap();
    assert!(runner.logs.join("scenario.json").is_file());
    fs::remove_dir_all(&runner.logs).unwrap();
}
#[test]
fn gui_power_scenario_can_cross_shutdown_and_verify_startup_key_reports() {
    let mut runner = test_runner(
        "[[step]]\naction='press'\ncontrol='power'\n[[step]]\naction='expect-powered'\npowered=false\n[[step]]\naction='press'\ncontrol='rec-record'\n[[step]]\naction='press'\ncontrol='power'\n[[step]]\naction='expect-startup-report'\ncontrol='rec-record'\n",
    );
    let mut data = PanelData {
        powered: true,
        ..Default::default()
    };
    assert!(matches!(
        runner.tick_without_input(data).unwrap(),
        Progress::Dispatch(Command::Press { target: 53 })
    ));

    data.powered = false;
    assert!(matches!(
        runner.tick_without_input(data).unwrap(),
        Progress::Dispatch(Command::Press { target: 24 })
    ));
    data.switches[24] = true;
    assert!(matches!(
        runner.tick_without_input(data).unwrap(),
        Progress::Dispatch(Command::Press { target: 53 })
    ));

    runner.guest_restarted(&runner.logs.clone());
    let [press, release] = trace::button_packets(Model::L6, "rec-record").unwrap();
    fs::write(
        runner.logs.join("engine.trace"),
        format!("{press}{release}"),
    )
    .unwrap();
    data.powered = true;
    assert!(matches!(
        runner.tick_without_input(data).unwrap(),
        Progress::Complete
    ));
    fs::remove_dir_all(&runner.logs).unwrap();
}
#[test]
fn gui_power_hold_can_finish_when_shutdown_prevents_release_acknowledgement() {
    let mut runner = test_runner(
        "[[step]]\naction='hold'\ncontrol='power'\nduration_ms=4000\n[[step]]\naction='expect-powered'\npowered=false\n",
    );
    let now = Instant::now();
    assert!(matches!(
        runner.advance(sample(0), now).unwrap(),
        Progress::Dispatch(Command::HoldDown {
            target: 53,
            duration_ms: 4000
        })
    ));
    let mut release = sample(4000);
    release.main.sent = 1;
    release.main.applied = 1;
    assert!(matches!(
        runner.advance(release.clone(), now).unwrap(),
        Progress::Dispatch(Command::Release { target: 53 })
    ));
    release.data.powered = false;
    release.main.powered_off = true;
    release.panel.powered_off = true;
    assert!(matches!(
        runner.advance(release, now).unwrap(),
        Progress::Complete
    ));
    fs::remove_dir_all(&runner.logs).unwrap();
}
#[test]
fn guest_time_and_wall_deadlines_are_independent() {
    let mut runner = test_runner("[[step]]\naction='delay'\nguest_ms=1000\nwall_timeout_s=2\n");
    let now = Instant::now();
    assert!(matches!(
        runner.advance(sample(500), now).unwrap(),
        Progress::Waiting
    ));
    assert!(matches!(
        runner
            .advance(sample(1499), now + Duration::from_millis(1900))
            .unwrap(),
        Progress::Waiting
    ));
    assert!(matches!(
        runner
            .advance(sample(1500), now + Duration::from_millis(1901))
            .unwrap(),
        Progress::Complete
    ));
    fs::remove_dir_all(&runner.logs).unwrap();
    let mut stalled = test_runner("[[step]]\naction='delay'\nguest_ms=1000\nwall_timeout_s=2\n");
    stalled.advance(sample(0), now).unwrap();
    assert!(
        stalled
            .advance(sample(0), now + Duration::from_secs(3))
            .unwrap_err()
            .contains("timed out")
    );
    fs::remove_dir_all(&stalled.logs).unwrap();
}

#[test]
fn saved_l6_pad_readiness_is_model_scoped_and_dispatches() {
    let prefix = "version=1\nname='pad-ready'\nmodels=['l6']\nbackends=['headless']\n[[step]]\naction='wait-l6-sound-pad-file'\n";
    let scenario =
        Scenario::parse(&format!("{prefix}path='SOUND_PAD/PAD1/TONE220.WAV'\n")).unwrap();
    let mut runner = Runner::new(
        scenario,
        Model::L6,
        std::env::temp_dir().join(format!(
            "smoke-pad-ready-{}",
            NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )),
    )
    .unwrap();
    assert!(matches!(
        runner.advance(sample(0), Instant::now()).unwrap(),
        Progress::Device(devices::Operation::WaitL6SoundPadFile { .. })
    ));
    fs::remove_dir_all(&runner.logs).unwrap();
    assert!(Scenario::parse(&format!("{prefix}path='../bad.wav'\n")).is_err());
    assert!(
        Scenario::parse(&format!(
            "{}path='SOUND_PAD/PAD1/TONE220.WAV'\n",
            prefix.replace("'l6'", "'l6max'")
        ))
        .is_err()
    );
}

#[test]
fn chord_requires_one_condition_and_factory_mode_is_l6max_only() {
    let prefix = "version=1\nname='chord'\nmodels=['l6max']\nbackends=['headless']\n[[step]]\naction='hold-chord'\ncontrols=['sound-pad-1']\n";
    Scenario::parse(&format!("{prefix}until='factory-mode'\n")).unwrap();
    assert!(Scenario::parse(&format!("{prefix}duration_ms=1000\n")).is_ok());
    assert!(Scenario::parse(prefix).is_err());
    assert!(Scenario::parse(&format!("{prefix}duration_ms=1000\nuntil='factory-mode'\n")).is_err());
    assert!(
        Scenario::parse(&format!(
            "{}until='factory-mode'\n",
            prefix.replace("'l6max'", "'l6'")
        ))
        .is_err()
    );
}

#[test]
fn effect_mode_wait_requires_a_target() {
    let prefix = "version=1\nname='effect-wait'\nmodels=['l6max']\nbackends=['headless']\n[[step]]\naction='wait-effect-mode'\n";
    assert!(Scenario::parse(prefix).is_err());
    assert!(Scenario::parse(&format!("{prefix}edit_mode=true\n")).is_ok());
    assert!(Scenario::parse(&format!("{prefix}panel_mode=6\n")).is_ok());
    assert!(
        Scenario::parse(&format!(
            "{}edit_mode=true\n",
            prefix.replace("l6max", "l6")
        ))
        .is_err()
    );
}

#[test]
fn wait_recorded_files_validates_names_and_dispatches() {
    let mut runner = test_runner(
        "backends=['headless']\n[[step]]\naction='wait-recorded-files'\nfiles=['TRACK01.WAV']\n",
    );
    assert!(matches!(
        runner.advance(sample(0), Instant::now()).unwrap(),
        Progress::Device(devices::Operation::WaitRecordedFiles { .. })
    ));
    fs::remove_dir_all(&runner.logs).unwrap();
    assert!(Scenario::parse("version=1\nname='invalid'\nmodels=['l6']\nbackends=['headless']\n[[step]]\naction='wait-recorded-files'\nfiles=['../outside']\n").is_err());
}

#[test]
fn wait_audio_input_dispatches_transport_condition() {
    let mut runner = test_runner(
        "backends=['headless']\n[[step]]\naction='wait-audio-input'\nplaying=true\nmin_ms=1000\nmax_ms=2000\n",
    );
    assert!(matches!(
        runner.advance(sample(0), Instant::now()).unwrap(),
        Progress::Device(devices::Operation::WaitAudioInput {
            playing: true,
            min_ms: 1000,
            max_ms: 2000
        })
    ));
    fs::remove_dir_all(&runner.logs).unwrap();
    assert!(Scenario::parse("version=1\nname='invalid'\nmodels=['l6']\nbackends=['headless']\n[[step]]\naction='wait-audio-input'\nplaying=false\nmin_ms=2\nmax_ms=1\n").is_err());
}

#[test]
fn wait_memory_dispatches_with_a_resolved_address() {
    let mut runner = test_runner(
        "backends=['headless']\n[[step]]\naction='wait-memory'\nsymbol='rec_record_lamp'\nbytes=[16,0,0,0]\n",
    );
    assert!(matches!(
        runner.advance(sample(0), Instant::now()).unwrap(),
        Progress::Device(devices::Operation::WaitMemory { .. })
    ));
    fs::remove_dir_all(&runner.logs).unwrap();
}

#[test]
fn sd_patch_empty_wait_dispatches_to_loader_check() {
    let mut runner = test_runner("[[step]]\naction='wait'\nuntil='sd-patch-empty'\n");
    assert!(matches!(
        runner.advance(sample(0), Instant::now()).unwrap(),
        Progress::Device(devices::Operation::WaitSdPatchEmpty)
    ));
    fs::remove_dir_all(&runner.logs).unwrap();
}

#[test]
fn sd_patch_loaded_wait_dispatches_to_loader_check() {
    let mut runner = test_runner("[[step]]\naction='wait'\nuntil='sd-patch-loaded'\n");
    assert!(matches!(
        runner.advance(sample(0), Instant::now()).unwrap(),
        Progress::Device(devices::Operation::WaitSdPatchLoaded)
    ));
    fs::remove_dir_all(&runner.logs).unwrap();
}

#[test]
fn activity_observation_uses_its_test_defined_guest_duration() {
    let mut runner = test_runner(
        "[[step]]\naction='observe-duration'\nactivity='recording'\nguest_ms=1000\nwall_timeout_s=3\n",
    );
    let now = Instant::now();
    assert!(matches!(
        runner.advance(sample(200), now).unwrap(),
        Progress::Waiting
    ));
    assert!(matches!(
        runner
            .advance(sample(1199), now + Duration::from_secs(2))
            .unwrap(),
        Progress::Waiting
    ));
    assert!(matches!(
        runner
            .advance(sample(1200), now + Duration::from_secs(2))
            .unwrap(),
        Progress::Complete
    ));
    fs::remove_dir_all(&runner.logs).unwrap();
    assert!(Scenario::parse("version=1\nname='bad-observation'\nmodels=['l6']\n[[step]]\naction='observe-duration'\nactivity='recording'\nguest_ms=11000\n").is_err());
}
#[test]
fn duplicate_reports_and_unrelated_ring_changes_are_failures() {
    for unrelated in [false, true] {
        let mut runner = test_runner(
            "[[step]]\naction='encoder'\nchannel=1\nsteps=12\n[[step]]\naction='expect-ring'\nchannel=1\nmask=31\nonly=true\n",
        );
        let now = Instant::now();
        runner.advance(sample(0), now).unwrap();
        let mut feedback = sample(100);
        feedback.panel.sent = 1;
        feedback.panel.applied = 1;
        feedback.reports = if unrelated { 12 } else { 13 };
        if unrelated {
            let (row, column) = l6max_host::indicators::RING_PINS[1][0];
            feedback.data.indicators[row] |= 1 << (column ^ 7);
        }
        assert!(runner.advance(feedback, now).is_err());
        fs::remove_dir_all(&runner.logs).unwrap();
    }
}
#[test]
fn explicit_up_is_acked_with_applied_not_the_automatic_tap_release_counter() {
    let mut runner = test_runner("[[step]]\naction='press'\ncontrol='channel.1.mute'\n");
    let now = Instant::now();
    assert!(matches!(
        runner.advance(sample(0), now).unwrap(),
        Progress::Dispatch(_)
    ));
    let mut ack = sample(100);
    ack.panel.sent = 2;
    ack.panel.applied = 1;
    assert!(matches!(
        runner.advance(ack.clone(), now).unwrap(),
        Progress::Waiting
    ));
    ack.panel.applied = 2;
    assert_eq!(ack.panel.released, 0);
    assert!(matches!(
        runner.advance(ack, now).unwrap(),
        Progress::Complete
    ));
    fs::remove_dir_all(&runner.logs).unwrap();
}
#[test]
fn scene_save_holds_until_the_lamp_changes_then_checks_both_panel_edges() {
    let mut runner = test_runner("[[step]]\naction='save-scene'\nscene=1\n");
    let now = Instant::now();
    assert!(matches!(
        runner.advance(sample(0), now).unwrap(),
        Progress::Dispatch(Command::HoldDown {
            target: 15,
            duration_ms: 50
        })
    ));

    let [press, release] = trace::button_packets(Model::L6, "scene-a").unwrap();
    let mut held = sample(100);
    held.panel.sent = 1;
    held.panel.applied = 1;
    held.traces.insert(press, 1);
    held.data.indicators[2] |= 1 << 11;
    assert!(matches!(
        runner.advance(held.clone(), now).unwrap(),
        Progress::Dispatch(Command::Release { target: 15 })
    ));

    held.panel.sent = 2;
    held.panel.applied = 2;
    held.traces.insert(release, 1);
    assert!(matches!(
        runner.advance(held, now).unwrap(),
        Progress::Complete
    ));
    fs::remove_dir_all(&runner.logs).unwrap();
}
#[test]
fn l6_button_lamps_in_unused_generic_ring_slots_are_not_other_encoders() {
    let mut runner = test_runner(
        "[[step]]\naction='encoder'\nchannel=1\nsteps=12\n[[step]]\naction='expect-ring'\nchannel=1\nmask=31\nonly=true\n",
    );
    let now = Instant::now();
    runner.advance(sample(0), now).unwrap();
    let mut obs = sample(100);
    obs.panel.sent = 1;
    obs.panel.applied = 1;
    obs.reports = 12;
    for &(row, column) in l6max_host::indicators::RING_PINS[0].iter().take(5) {
        obs.data.indicators[row] |= 1 << (column ^ 7);
    }
    let (row, column) = l6max_host::indicators::RING_PINS[6][7];
    obs.data.indicators[row] |= 1 << (column ^ 7);
    assert!(matches!(
        runner.advance(obs.clone(), now).unwrap(),
        Progress::Waiting
    ));
    obs.panel.guest_ms = 1100;
    assert!(matches!(
        runner.advance(obs, now).unwrap(),
        Progress::Complete
    ));
    fs::remove_dir_all(&runner.logs).unwrap();
}
#[test]
fn restored_mixer_values_must_cover_all_visible_channels_and_light_the_mode_lamp() {
    let mut runner = test_runner("[[step]]\naction='wait'\nuntil='mixer-ready'\n");
    let now = Instant::now();
    let mut obs = sample(0);
    let (row, column) = l6max_host::indicators::RING_PINS[0][0];
    obs.data.indicators[row] |= 1 << (column ^ 7);
    runner.advance(obs.clone(), now).unwrap();
    obs.panel.guest_ms = 1000;
    assert!(matches!(
        runner.advance(obs.clone(), now).unwrap(),
        Progress::Waiting
    ));
    for pins in l6max_host::indicators::RING_PINS.iter().take(6) {
        let (row, column) = pins[0];
        obs.data.indicators[row] |= 1 << (column ^ 7);
    }
    obs.panel.guest_ms = 2000;
    assert!(matches!(
        runner.advance(obs.clone(), now).unwrap(),
        Progress::Waiting
    ));
    obs.data.indicators[4] |= 1 << 8;
    obs.panel.guest_ms = 2050;
    assert!(matches!(
        runner.advance(obs, now).unwrap(),
        Progress::Complete
    ));
    fs::remove_dir_all(&runner.logs).unwrap();
}
#[test]
fn parser_rejects_typos_and_unavailable_model_controls() {
    let prefix = "version=1\nname='test'\nmodels=['l6']\n";
    assert!(
        Scenario::parse(&format!(
            "{prefix}[[step]]\naction='press'\ncontrol='recorder.down'\n"
        ))
        .is_err()
    );
    assert!(
        Scenario::parse(&format!(
            "{prefix}[[step]]\naction='encoder'\nchannel=8\nsteps=12\n"
        ))
        .is_err()
    );
    assert!(
        Scenario::parse(&format!(
            "{prefix}[[step]]\naction='wait'\nuntil='boot-ready'\ntypo=true\n"
        ))
        .is_err()
    );
    assert!(Scenario::parse(&format!("{prefix}[[step]]\naction='save-scene'\nscene=1\n")).is_ok());
    assert!(Scenario::parse(&format!("{prefix}[[step]]\naction='save-scene'\nscene=4\n")).is_err());
    let l6max = prefix.replace("['l6']", "['l6max']");
    assert!(Scenario::parse(&format!("{l6max}[[step]]\naction='save-scene'\nscene=1\n")).is_err());
}

#[test]
fn usb_sysex_scenario_requires_scoped_complete_messages() {
    let prefix = "version=1\nname='sysex'\nmodels=['l6max']\nbackends=['headless']\n[[step]]\naction='usb-descriptors'\n[[step]]\naction='usb-sysex-begin'\nguest_timeout_ms=1000\n";
    let suffix = "[[step]]\naction='usb-sysex-end'\n";
    let good = format!(
        "{prefix}[[step]]\naction='usb-sysex-exchange'\nbytes=[240,1,247]\nexpect_prefix=[240,1]\n{suffix}"
    );
    assert!(Scenario::parse(&good).is_ok());
    let invalid = good.replace("[240,1,247]", "[240,1,2]");
    assert!(
        Scenario::parse(&invalid)
            .unwrap_err()
            .contains("complete F0..F7")
    );
    let invalid = good.replace("expect_prefix=[240,1]", "reject_prefixes=[[]]");
    assert!(
        Scenario::parse(&invalid)
            .unwrap_err()
            .contains("reject_prefixes")
    );
    assert!(
        Scenario::parse(&good.replace(suffix, ""))
            .unwrap_err()
            .contains("unterminated")
    );
    let invalid = good.replace("expect_prefix=[240,1]", "reply_timeout_ms=1001");
    assert!(
        Scenario::parse(&invalid)
            .unwrap_err()
            .contains("exceeds campaign")
    );
}
#[test]
fn editor_pad_assignment_requires_prior_usb_midi_enumeration() {
    let prefix = "version=1\nname='pads'\nmodels=['l6max']\nbackends=['headless']\n";
    let without_usb = Scenario::parse(&format!(
        "{prefix}[[step]]\naction='editor-pad-assignments'\n"
    ));
    assert!(
        without_usb
            .unwrap_err()
            .contains("requires a preceding usb-descriptors step")
    );
    let with_usb = Scenario::parse(&format!(
        "{prefix}[[step]]\naction='usb-descriptors'\n[[step]]\naction='editor-pad-assignments'\n"
    ));
    assert!(with_usb.is_ok());
    let playback = Scenario::parse(&format!(
        "{prefix}[[step]]\naction='usb-descriptors'\n[[step]]\naction='editor-pad-assignments'\n[[step]]\naction='editor-pad-playback'\npad=3\n"
    ));
    assert!(playback.is_err(), "Editor SysEx playback is L6-only");
    let l6_prefix = prefix.replace("l6max", "l6");
    let playback = Scenario::parse(&format!(
        "{l6_prefix}[[step]]\naction='usb-descriptors'\n[[step]]\naction='editor-pad-assignments'\n[[step]]\naction='editor-pad-playback'\npad=3\n"
    ));
    assert!(playback.is_ok());
    let invalid_pad = Scenario::parse(&format!(
        "{l6_prefix}[[step]]\naction='usb-descriptors'\n[[step]]\naction='editor-pad-assignments'\n[[step]]\naction='editor-pad-playback'\npad=4\n"
    ));
    assert!(invalid_pad.unwrap_err().contains("pad must be 0..3"));
    let ui_playback = Scenario::parse(&format!(
        "{prefix}[[step]]\naction='usb-descriptors'\n[[step]]\naction='l6max-ui-pad-verify'\npad=2\n"
    ));
    assert!(ui_playback.is_ok());
    let wrong_model = prefix.replace("l6max", "l6");
    let wrong_model = Scenario::parse(&format!(
        "{wrong_model}[[step]]\naction='usb-descriptors'\n[[step]]\naction='l6max-ui-pad-verify'\npad=2\n"
    ));
    assert!(wrong_model.unwrap_err().contains("scoped to L6max"));
}
#[test]
fn sound_pad_playback_supports_editor_or_saved_assignment() {
    let prefix = "version=1\nname='pad-playback'\nmodels=['l6']\nbackends=['headless']\n";
    let action = "[[step]]\naction='usb-descriptors'\n[[step]]\naction='editor-pad-assignments'\n[[step]]\naction='sound-pad-playback'\npad=0\ntarget=49\n";
    assert!(Scenario::parse(&format!("{prefix}{action}")).is_ok());
    let l6max = prefix.replace("models=['l6']", "models=['l6max']");
    assert!(Scenario::parse(&format!("{l6max}{action}")).is_ok());
    let without_assignment = Scenario::parse(&format!(
        "{prefix}[[step]]\naction='sound-pad-playback'\npad=0\ntarget=49\n"
    ));
    assert!(
        without_assignment
            .unwrap_err()
            .contains("preceding editor-pad-assignments")
    );
    let saved_state = Scenario::parse(&format!(
        "{prefix}[[step]]\naction='sound-pad-playback'\npad=0\ntarget=49\nassignment='saved-state'\nplayback_window_ms=30000\nrequire_pad_led_change=false\n"
    ));
    assert!(saved_state.is_ok());
    let invalid_target = Scenario::parse(&format!(
        "{prefix}[[step]]\naction='usb-descriptors'\n[[step]]\naction='editor-pad-assignments'\n[[step]]\naction='sound-pad-playback'\npad=0\ntarget=53\n"
    ));
    assert!(
        invalid_target
            .unwrap_err()
            .contains("target must match pad")
    );
    let mismatched_target = Scenario::parse(&format!(
        "{prefix}[[step]]\naction='usb-descriptors'\n[[step]]\naction='editor-pad-assignments'\n[[step]]\naction='sound-pad-playback'\npad=1\ntarget=49\n"
    ));
    assert!(
        mismatched_target
            .unwrap_err()
            .contains("target must match pad")
    );
}

#[test]
fn l6_recorder_playback_is_scoped_to_the_verified_play_gpio() {
    let prefix = "version=1\nname='recorder-playback'\nmodels=['l6']\nbackends=['headless']\n";
    let scenario = format!("{prefix}[[step]]\naction='l6-recorder-playback'\ntarget=4\n");
    assert!(Scenario::parse(&scenario).is_ok());
    assert!(
        Scenario::parse(&scenario.replace("target=4", "target=5"))
            .unwrap_err()
            .contains("MAIN target 4")
    );
    assert!(
        Scenario::parse(&scenario.replace("models=['l6']", "models=['l6max']"))
            .unwrap_err()
            .contains("scoped to L6")
    );
}
#[test]
fn l6max_recorder_playback_is_scoped_to_l6max_and_its_gpio() {
    let prefix = "version=1
name='recorder-playback'
models=['l6max']
backends=['headless']
";
    let scenario = format!(
        "{prefix}[[step]]
action='l6max-recorder-playback'
target=4
"
    );
    assert!(Scenario::parse(&scenario).is_ok());
    assert!(Scenario::parse(&scenario.replace("target=4", "target=5")).is_err());
    assert!(Scenario::parse(&scenario.replace("models=['l6max']", "models=['l6']")).is_err());
}
#[test]
fn unsupported_schema_and_zero_timeouts_fail_before_boot() {
    let text =
        "version=1\nname='test'\nmodels=['l6max']\n[[step]]\naction='wait'\nuntil='boot-ready'\n";
    assert!(Scenario::parse(text).is_ok());
    assert!(Scenario::parse(&text.replace("version=1", "version=2")).is_err());
    assert!(Scenario::parse(&format!("{text}wall_timeout_s=0\n")).is_err());
}
#[test]
fn l6_main_gpio_event_scenario_rejects_wrong_model_and_mapping() {
    let base = "version=1\nname='gpio'\nmodels=['l6']\nbackends=['headless']\n[[step]]\naction='l6-main-gpio-event'\ntarget=48\nevent=25\n";
    assert!(Scenario::parse(base).is_ok());
    assert!(Scenario::parse(&base.replace("models=['l6']", "models=['l6max']")).is_err());
    assert!(Scenario::parse(&base.replace("event=25", "event=26")).is_err());
    assert!(Scenario::parse(&base.replace("target=48", "target=53")).is_err());
}
#[test]
fn l6_usb_startup_requires_first_step_and_l6_scope() {
    let base = "version=1
name='usb'
models=['l6']
backends=['headless']
[[step]]
action='l6-usb-startup'
";
    assert!(Scenario::parse(base).is_ok());
    assert!(Scenario::parse(&base.replace("models=['l6']", "models=['l6max']")).is_err());
    let late = base.replace(
        "[[step]]
action='l6-usb-startup'",
        "[[step]]
action='delay'
guest_ms=1
[[step]]
action='l6-usb-startup'",
    );
    assert!(Scenario::parse(&late).is_err());
}
#[test]
fn l6_din_sweep_is_scoped_to_l6() {
    let base = "version=1
name='din-sweep'
models=['l6']
backends=['headless']
[[step]]
action='l6-din-sweep'
";
    assert!(Scenario::parse(base).is_ok());
    assert!(Scenario::parse(&base.replace("models=['l6']", "models=['l6max']")).is_err());
}
#[test]
fn every_checked_in_scenario_loads_after_composition() {
    // Case files live under `tests/emulator/cases` and each patch's
    // `tests/emulator/cases`; those with only a `[case]` table have no
    // scenario of their own.
    fn case_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for path in fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()) {
            if path.is_dir() {
                case_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "toml") {
                out.push(path);
            }
        }
    }
    let root = l6max_host::paths::repository_root();
    let mut paths = Vec::new();
    case_files(&root.join("tests/emulator/cases"), &mut paths);
    for patch in fs::read_dir(root.join("patches")).unwrap() {
        let cases = patch.unwrap().path().join("tests/emulator/cases");
        if cases.is_dir() {
            case_files(&cases, &mut paths);
        }
    }
    assert!(paths.len() > 80);
    for path in paths {
        let table: toml::Table = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        if table.keys().all(|key| key == "case") {
            continue;
        }
        let scenario = Scenario::load(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(
            !scenario
                .step
                .iter()
                .any(|s| matches!(s.action, Action::Include { .. }))
        );
        for model in &scenario.models {
            let mut runner = test_runner("[[step]]\naction='delay'\nguest_ms=1\n");
            let temp = runner.logs.clone();
            runner = Runner::new(scenario.clone(), *model, temp)
                .unwrap_or_else(|e| panic!("{} / {model}: {e}", path.display()));
            fs::remove_dir_all(runner.logs).unwrap();
        }
    }
}
/// A directory under the repository's ignored `target/`, as an absolute
/// path and as the root-relative path includes name it by.
fn include_test_dir(name: &str) -> (PathBuf, String) {
    let rel = format!(
        "target/smoke-core-include-tests/{name}-{}",
        std::process::id()
    );
    let dir = l6max_host::paths::repository_root().join(&rel);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    (dir, rel)
}
#[test]
fn includes_are_confined_cycle_checked_and_preserve_model_scope() {
    let (dir, rel) = include_test_dir("scope");
    fs::write(
        dir.join("fragment.toml"),
        "[[step]]\naction='delay'\nguest_ms=2\n",
    )
    .unwrap();
    let prefix = format!(
        "version=1\nname='include'\nmodels=['l6','l6max']\n[[step]]\nmodels=['l6max']\naction='include'\npath='{rel}/fragment.toml'\n"
    );
    fs::write(dir.join("test.toml"), &prefix).unwrap();
    let loaded = Scenario::load(&dir.join("test.toml")).unwrap();
    assert_eq!(loaded.step[0].models, [Model::L6max]);
    // A generated scenario composes the same way from its text alone.
    assert_eq!(Scenario::from_text(&prefix).unwrap().step.len(), 1);
    fs::write(
        dir.join("fragment.toml"),
        format!("[[step]]\naction='include'\npath='{rel}/fragment.toml'\n"),
    )
    .unwrap();
    assert!(
        Scenario::load(&dir.join("test.toml"))
            .unwrap_err()
            .contains("cycle")
    );
    for path in [
        format!("{rel}/../outside.toml"),
        "fragment.toml/..".into(),
        dir.join("fragment.toml").display().to_string(),
    ] {
        let text = prefix.replace(&format!("{rel}/fragment.toml"), &path);
        assert!(
            Scenario::from_text(&text)
                .unwrap_err()
                .contains("relative to the repository root"),
            "{path}"
        );
    }
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn include_deadline_overrides_preserve_scope_and_reject_mixed_deadlines() {
    let (dir, rel) = include_test_dir("deadline-override");
    fs::write(dir.join("inner.toml"), "[[step]]\naction='delay'\nguest_ms=2\nguest_timeout_ms=100\nwall_timeout_s=5\n[[step]]\naction='delay'\nguest_ms=3\nguest_timeout_ms=100\nwall_timeout_s=5\n").unwrap();
    fs::write(
        dir.join("outer.toml"),
        format!("[[step]]\naction='include'\npath='{rel}/inner.toml'\n"),
    )
    .unwrap();
    let source = format!(
        "version=1\nname='include'\nmodels=['l6max']\n[[step]]\nmodels=['l6max']\nsetup_input=true\naction='include'\npath='{rel}/outer.toml'\ninclude_guest_timeout_ms=200\ninclude_wall_timeout_s=9\n"
    );
    let scenario = Scenario::from_text(&source).unwrap();
    assert_eq!(scenario.step.len(), 2);
    assert!(
        scenario
            .step
            .iter()
            .all(|step| step.models == [Model::L6max]
                && step.setup_input
                && step.guest_timeout_ms == 200
                && step.wall_timeout_s == 9)
    );
    assert!(
        Scenario::from_text(
            &source.replace("include_guest_timeout_ms=200", "include_guest_timeout_ms=0")
        )
        .is_err()
    );
    fs::write(dir.join("inner.toml"), "[[step]]\naction='delay'\nguest_ms=2\nguest_timeout_ms=100\nwall_timeout_s=5\n[[step]]\naction='delay'\nguest_ms=3\nguest_timeout_ms=99\nwall_timeout_s=5\n").unwrap();
    assert!(
        Scenario::from_text(&source)
            .unwrap_err()
            .contains("uniform child")
    );
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn l6max_tap_tempo_lamp_does_not_block_panel_settling() {
    let mut rows = [0x28, 0x1000, 0x40002, 0x8000, 0x110, 4, 1, 0];
    let blinking = {
        let mut r = rows;
        r[1] ^= 0x20000;
        r
    };
    assert_eq!(
        settling_rows(Model::L6max, rows),
        settling_rows(Model::L6max, blinking)
    );
    assert_ne!(
        settling_rows(Model::L6, rows),
        settling_rows(Model::L6, blinking)
    );
    rows[1] ^= 0x10000;
    assert_ne!(
        settling_rows(Model::L6max, rows),
        settling_rows(Model::L6max, blinking)
    );
}
#[test]
fn setup_input_propagates_through_includes_and_is_dropped_for_spec_state() {
    let (dir, rel) = include_test_dir("setup");
    fs::write(
        dir.join("fragment.toml"),
        "[[step]]\naction='delay'\nguest_ms=2\n[[step]]\naction='delay'\nguest_ms=3\n",
    )
    .unwrap();
    let text = format!(
        "version=1\nname='setup'\nmodels=['l6max']\n[[step]]\nsetup_input=true\naction='include'\npath='{rel}/fragment.toml'\n[[step]]\naction='delay'\nguest_ms=4\n"
    );
    let loaded = Scenario::from_text(&text).unwrap();
    assert_eq!(
        loaded
            .step
            .iter()
            .map(|s| s.setup_input)
            .collect::<Vec<_>>(),
        [true, true, false]
    );
    let kept = loaded.without_setup_input().unwrap();
    assert_eq!(kept.step.len(), 1);
    assert!(matches!(kept.step[0].action, Action::Delay { guest_ms: 4 }));
    fs::remove_dir_all(&dir).unwrap();
}
#[test]
fn unsupported_operations_and_switch_semantics_fail_before_boot() {
    let prefix = "version=1\nname='invalid'\nmodels=['l6max']\n";
    for step in [
        "action='usb-descriptors'",
        "action='press'\ncontrol='ch5-pad-switch'",
        "action='switch'\ncontrol='channel.1.mute'\nenabled=true",
        "action='expect-ring'\nchannel=1\nmask=1024",
        "action='expect-ring'\nchannel=1\nchanged=true\nsettle_ms=0",
        "action='checkpoint'\nname='../escape'",
        "action='expect-display'\ncheckpoint='missing'\nchanged=true",
    ] {
        assert!(
            Scenario::parse(&format!("{prefix}[[step]]\n{step}\n")).is_err(),
            "{step}"
        );
    }
}

#[test]
fn typed_input_sweep_allows_only_its_verified_rejections() {
    let now = Instant::now();
    let mut runner = test_runner("backends=['headless']\n[[step]]\naction='l6-input-sweep'\n");
    runner.device_finished(json!({"expected_rejections":{"main":2,"panel":2}}));
    let mut obs = sample(0);
    obs.main.rejected = 2;
    obs.panel.rejected = 2;
    assert!(matches!(
        runner.advance(obs, now).unwrap(),
        Progress::Complete
    ));
    fs::remove_dir_all(&runner.logs).unwrap();

    let mut runner = test_runner("backends=['headless']\n[[step]]\naction='l6-input-sweep'\n");
    runner.device_finished(json!({"expected_rejections":{"main":2,"panel":2}}));
    let mut obs = sample(0);
    obs.main.rejected = 3;
    obs.panel.rejected = 2;
    assert!(runner.advance(obs, now).is_err());
    fs::remove_dir_all(&runner.logs).unwrap();
}
#[test]
fn wire_edges_wait_for_release_and_reject_duplicate_edges() {
    let text = "version=1\nname='edges'\nmodels=['l6max']\n[[step]]\naction='press'\ncontrol='scene-a'\n[[step]]\naction='expect-button-report'\ncontrol='scene-a'\n";
    for duplicate in [false, true] {
        let mut runner = test_runner("[[step]]\naction='delay'\nguest_ms=1\n");
        runner.scenario = Scenario::parse(text).unwrap();
        runner.model = Model::L6max;
        let now = Instant::now();
        runner.advance(sample(0), now).unwrap();
        let mut obs = sample(100);
        obs.panel.sent = 2;
        obs.panel.applied = 2;
        let [on, off] = trace::button_packets(Model::L6max, "scene-a").unwrap();
        obs.traces.insert(on, if duplicate { 2 } else { 1 });
        if duplicate {
            assert!(runner.advance(obs, now).unwrap_err().contains("duplicate"));
        } else {
            assert!(matches!(
                runner.advance(obs.clone(), now).unwrap(),
                Progress::Waiting
            ));
            obs.traces.insert(off, 1);
            obs.panel.guest_ms = 1200;
            assert!(matches!(
                runner.advance(obs, now).unwrap(),
                Progress::Complete
            ));
        }
        fs::remove_dir_all(&runner.logs).unwrap();
    }
}
#[test]
fn late_ring_feedback_resets_the_declared_quiet_window() {
    let mut runner = test_runner(
        "[[step]]\naction='encoder'\nchannel=1\nsteps=12\n[[step]]\naction='expect-ring'\nchannel=1\nchanged=true\nonly=true\nsettle_ms=3000\n",
    );
    let now = Instant::now();
    runner.advance(sample(0), now).unwrap();
    let mut obs = sample(100);
    obs.panel.sent = 1;
    obs.panel.applied = 1;
    obs.reports = 12;
    let (row, col) = l6max_host::indicators::RING_PINS[0][0];
    obs.data.indicators[row] |= 1 << (col ^ 7);
    runner.advance(obs.clone(), now).unwrap();
    obs.panel.guest_ms = 1100;
    assert!(matches!(
        runner.advance(obs.clone(), now).unwrap(),
        Progress::Waiting
    ));
    let (row, col) = l6max_host::indicators::RING_PINS[0][1];
    obs.data.indicators[row] |= 1 << (col ^ 7);
    obs.panel.guest_ms = 2000;
    runner.advance(obs.clone(), now).unwrap();
    obs.panel.guest_ms = 4999;
    assert!(matches!(
        runner.advance(obs.clone(), now).unwrap(),
        Progress::Waiting
    ));
    obs.panel.guest_ms = 5000;
    assert!(matches!(
        runner.advance(obs, now).unwrap(),
        Progress::Complete
    ));
    fs::remove_dir_all(&runner.logs).unwrap();
}
#[test]
fn unexpected_power_loss_fails_but_explicit_restart_accepts_shutdown() {
    let now = Instant::now();
    for restart in [false, true] {
        let mut runner = test_runner(if restart {
            "backends=['headless']\n[[step]]\naction='restart'\n"
        } else {
            "[[step]]\naction='delay'\nguest_ms=1000\n"
        });
        if restart {
            let mut skipped = runner.scenario.step[0].clone();
            skipped.models = vec![Model::L6max];
            skipped.action = Action::Delay { guest_ms: 1 };
            runner.scenario.step.insert(0, skipped);
        }
        let mut obs = sample(0);
        obs.data.powered = false;
        let result = runner.advance(obs, now);
        if restart {
            assert!(matches!(
                result.unwrap(),
                Progress::Device(devices::Operation::Restart)
            ));
        } else {
            assert!(result.is_err());
        }
        fs::remove_dir_all(&runner.logs).unwrap();
    }
}

#[test]
fn knob_dialog_requires_adjacent_readiness_with_matching_budget() {
    let prefix = "version=1\nname='knob'\nmodels=['l6max']\nbackends=['headless']\n";
    let ready =
        "[[step]]\naction='knob-dialog-ready'\nguest_timeout_ms=180000\nwall_timeout_s=600\n";
    let guard =
        "[[step]]\naction='knob-dialog-guard'\nguest_timeout_ms=180000\nwall_timeout_s=600\n";
    let baseline =
        "[[step]]\naction='knob-dialog-baseline'\nguest_timeout_ms=180000\nwall_timeout_s=600\n";
    let preflight = ["begin", "descriptor", "text-alias", "layer-collision", "widget-layer", "background-layer"].iter().map(|name| format!("[[step]]\naction='knob-dialog-preflight-{name}'\nguest_timeout_ms=180000\nwall_timeout_s=600\n")).collect::<String>();
    let activation = "[[step]]\naction='knob-dialog-callback-race'\nguest_timeout_ms=180000\nwall_timeout_s=600\n[[step]]\naction='knob-dialog-first-activation'\nguest_timeout_ms=180000\nwall_timeout_s=600\n";
    let active_demo = "[[step]]\naction='knob-dialog-active-display'\nguest_timeout_ms=180000\nwall_timeout_s=600\n[[step]]\naction='knob-dialog-level-demo'\nguest_timeout_ms=180000\nwall_timeout_s=600\n";
    let tasks = "[[step]]\naction='knob-dialog-task-contexts'\nguest_timeout_ms=180000\nwall_timeout_s=600\n";
    let overlay = "[[step]]\naction='knob-dialog-overlay-invariants'\nguest_timeout_ms=180000\nwall_timeout_s=600\n[[step]]\naction='knob-dialog-bitmap-zero'\nguest_timeout_ms=180000\nwall_timeout_s=600\n[[step]]\naction='knob-dialog-active-overlay'\nguest_timeout_ms=180000\nwall_timeout_s=600\n";
    let locked = "[[step]]\naction='knob-dialog-locked-update'\nguest_timeout_ms=180000\nwall_timeout_s=600\n";
    let dialog = "[[step]]\naction='knob-dialog'\nguest_timeout_ms=180000\nwall_timeout_s=600\n";
    let modes =
        include_str!("../../../../patches/knob-dialog/tests/emulator/fragments/mode-walk.toml");
    assert!(
        Scenario::parse(&format!(
            "{prefix}{ready}{guard}{baseline}{preflight}{activation}{active_demo}{tasks}{overlay}{locked}{modes}{dialog}"
        ))
        .is_ok()
    );
    assert!(Scenario::parse(&format!("{prefix}{ready}{guard}{baseline}{preflight}")).is_err());
    let complete = format!(
        "{prefix}{ready}{guard}{baseline}{preflight}{activation}{active_demo}{tasks}{overlay}{locked}{modes}{dialog}"
    );
    // The fragment supplies parameters; changing a valid input is permitted,
    // but the scoped sequence and final Pan mode remain required.
    assert!(Scenario::parse(&complete.replacen("hold_ms = 50", "hold_ms = 100", 1)).is_ok());
    assert!(
        Scenario::parse(&complete.replacen(
            "action = \"wait-input-applied\"",
            "action = \"knob-dialog-expect-heap\"",
            1
        ))
        .is_err()
    );
    assert!(Scenario::parse(&complete.replace("mode = 8", "mode = 7")).is_err());

    let setup_pruned =
        format!("{prefix}{ready}{guard}{baseline}{preflight}{activation}{active_demo}{tasks}{overlay}{locked}{modes}{dialog}")
            .replace(
                "action='knob-dialog-preflight-widget-layer'",
                "setup_input=true\naction='knob-dialog-preflight-widget-layer'",
            );
    assert!(
        Scenario::parse(&setup_pruned)
            .unwrap()
            .without_setup_input()
            .is_err()
    );
    assert!(Scenario::parse(&format!("{prefix}{ready}{dialog}")).is_err());
    assert!(Scenario::parse(&format!("{prefix}{dialog}")).is_err());
    assert!(
        Scenario::parse(&format!(
            "{prefix}{ready}[[step]]\naction='wait'\nuntil='boot-ready'\n{dialog}"
        ))
        .is_err()
    );
    assert!(
        Scenario::parse(&format!(
            "{prefix}{ready}{guard}{baseline}{preflight}{activation}{active_demo}{tasks}{overlay}{locked}{modes}{}",
            dialog.replace("wall_timeout_s=600", "wall_timeout_s=599")
        ))
        .is_err()
    );
}

#[test]
fn captured_display_primitives_reject_screenless_l6() {
    let prefix = "version=1\nname='display'\nmodels=['l6']\nbackends=['headless']\n";
    assert!(
        Scenario::parse(&format!(
            "{prefix}[[step]]\naction='capture-display'\nguest_ms=400\nartifact='capture'\n"
        ))
        .is_err()
    );
    assert!(Scenario::parse(&format!("{prefix}[[step]]\naction='expect-captured-display-image'\nimage='tests/emulator/references/l6max-recorder-home.png'\n")).is_err());
}

#[test]
fn completion_does_not_publish_pass_before_adapter_cleanup() {
    let mut runner = test_runner("[[step]]\naction='delay'\nguest_ms=1\n");
    let mut skipped = runner.scenario.step[0].clone();
    skipped.models = vec![Model::L6max];
    runner.scenario.step.push(skipped);
    let now = Instant::now();
    assert!(matches!(
        runner.advance(sample(0), now).unwrap(),
        Progress::Waiting
    ));
    assert!(matches!(
        runner.advance(sample(1), now).unwrap(),
        Progress::Complete
    ));
    assert!(!runner.logs.join("scenario.json").exists());
    runner.finish("FAIL", Some("cleanup failed")).unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(runner.logs.join("scenario.json")).unwrap()).unwrap();
    assert_eq!(report["outcome"], "FAIL");
    fs::remove_dir_all(&runner.logs).unwrap();
}

#[test]
fn midi_probe_requires_complete_model_scoped_transaction_and_matching_deadlines() {
    let prefix = "version=1\nname='probe'\nmodels=['l6max']\nbackends=['headless']\n";
    let begin = "[[step]]\naction='midi-probe-begin'\nguest_timeout_ms=10000\nwall_timeout_s=120\n";
    let send = "[[step]]\naction='midi-probe-send-cc'\nport='din'\nchannel=0\ncontroller=81\nvalue=40\ncapture_ring_channel=1\nguest_timeout_ms=10000\nwall_timeout_s=120\n";
    let expect = "[[step]]\naction='midi-probe-expect-ring'\nchannel=1\nchanged=false\nguest_ms=500\nwall_ms=3000\nguest_timeout_ms=10000\nwall_timeout_s=120\n";
    let end = "[[step]]\naction='midi-probe-end'\nguest_timeout_ms=10000\nwall_timeout_s=120\n";
    assert!(Scenario::parse(&format!("{prefix}{begin}{send}{expect}{end}")).is_ok());
    assert!(Scenario::parse(&format!("{prefix}{begin}{send}{expect}")).is_err());
    assert!(Scenario::parse(&format!("{prefix}{send}{end}")).is_err());
    assert!(
        Scenario::parse(&format!(
            "{prefix}{begin}{send}[[step]]\naction='delay'\nguest_ms=1\n{end}"
        ))
        .is_err()
    );
    assert!(
        Scenario::parse(&format!(
            "{prefix}{begin}{send}{}",
            end.replace("wall_timeout_s=120", "wall_timeout_s=121")
        ))
        .is_err()
    );
    let no_guest_window = expect.replace("guest_ms=500\n", "");
    assert!(Scenario::parse(&format!("{prefix}{begin}{send}{no_guest_window}{end}")).is_err());
    let tap = "[[step]]\naction='midi-probe-drain-and-tap'\nport='din'\ntarget=22\nhold_ms=50\nguest_timeout_ms=10000\nwall_timeout_s=120\n";
    let observe = "[[step]]\naction='midi-probe-expect-no-events'\nport='mixer'\nwall_ms=600\nguest_timeout_ms=10000\nwall_timeout_s=120\n";
    assert!(Scenario::parse(&format!("{prefix}{begin}{tap}{observe}{end}")).is_err());
    let pruned = format!(
        "{prefix}{begin}{}{expect}{}",
        send.replace(
            "action='midi-probe-send-cc'",
            "setup_input=true\naction='midi-probe-send-cc'"
        ),
        end
    );
    assert!(
        Scenario::parse(&pruned)
            .unwrap()
            .without_setup_input()
            .is_err()
    );
}

#[test]
fn midi_sync_requires_ordered_complete_scope_and_matching_deadlines() {
    let prefix = "version=1\nname='sync'\nmodels=['l6','l6max']\nbackends=['headless']\n";
    let begin = "[[step]]\naction='host-midi-sync-begin'\n";
    let drain = "[[step]]\naction='host-midi-drain'\nquiet_wall_ms=500\n";
    let send = "[[step]]\naction='host-midi-send-cc'\nchannel=0\ncontroller=127\nvalue=127\n";
    let expect = "[[step]]\naction='host-midi-expect-sync'\n";
    let end = "[[step]]\naction='host-midi-end'\n";
    assert!(Scenario::parse(&format!("{prefix}{begin}{drain}{send}{expect}{end}")).is_ok());
    assert!(Scenario::parse(&format!("{prefix}{begin}{expect}{end}")).is_ok());
    assert!(Scenario::parse(&format!("{prefix}{begin}{drain}{expect}{end}")).is_err());
    assert!(Scenario::parse(&format!("{prefix}{begin}{send}{expect}{end}")).is_err());
    assert!(Scenario::parse(&format!("{prefix}{begin}{expect}")).is_err());
    assert!(Scenario::parse(&format!("{prefix}{expect}{end}")).is_err());
    assert!(
        Scenario::parse(&format!(
            "{prefix}{begin}{}{end}",
            expect.replace(
                "action='host-midi-expect-sync'",
                "action='host-midi-expect-sync'\nwall_timeout_s=121"
            )
        ))
        .is_err()
    );
}

#[test]
fn update_lifecycle_requires_isolated_fixture_and_ordered_steps() {
    let prefix = "version=1\nname='bad-update'\nmodels=['l6max']\nbackends=['headless']\nstate='none'\ncard='none'\nupdate_fixture='bad-checksum'\n";
    let start = "[[step]]\naction='lifecycle-start'\nprofile='initial'\n";
    let wait = "[[step]]\naction='wait-panel-uptime'\nguest_ms=40000\nguest_timeout_ms=300000\n";
    let state = "[[step]]\naction='expect-update-state'\nmode=1\nflags_mask=9\nflags_value=0\n";
    let stop = "[[step]]\naction='lifecycle-stop'\n";
    let marker = "[[step]]\naction='expect-update-marker-absent'\n";
    assert!(Scenario::parse(&format!("{prefix}{start}{wait}{state}{stop}{marker}")).is_ok());
    assert!(
        Scenario::parse(
            &format!("{prefix}{start}{wait}{state}{stop}{marker}")
                .replace("guest_ms=40000", "guest_ms=1")
        )
        .is_err()
    );
    assert!(Scenario::parse(&format!("{prefix}{start}{wait}{state}{marker}")).is_err());
    assert!(
        Scenario::parse(
            &format!("{prefix}{start}{wait}{state}{stop}{marker}")
                .replace("models=['l6max']", "models=['l6']")
        )
        .is_err()
    );
    assert!(
        Scenario::parse(
            &format!("{prefix}{start}{wait}{state}{stop}{marker}")
                .replace("flags_value=0", "flags_value=2")
        )
        .is_err()
    );
    assert!(
        Scenario::parse(
            &format!("{prefix}{start}{wait}{state}{stop}{marker}")
                .replace("guest_ms=40000", "guest_ms=0")
        )
        .is_err()
    );
}

#[test]
fn synthetic_valid_handoff_requires_all_semantic_checks() {
    let fragment =
        include_str!("../../../../tests/emulator/fragments/l6max-update-valid-handoff.toml");
    let source = format!(
        "version=1\nname='handoff'\nmodels=['l6max']\nbackends=['headless']\nstate='none'\ncard='none'\nupdate_fixture='synthetic-valid'\nupdate_goal='handoff'\n{fragment}"
    );
    assert!(Scenario::parse(&source).is_ok());
    for missing in [
        "[[step]]\naction = \"expect-display-image\"\nimage = \"tests/smoke/images/l6max-firmware-update-dialog.png\"\nguest_timeout_ms = 60000\nwall_timeout_s = 300\n\n",
        "[[step]]\naction = \"expect-update-state\"\nmode = 6\nflags_mask = 9\nflags_value = 0\n\n",
        "[[step]]\naction = \"expect-update-gui\"\nwindow = 0x8020029c\nprogress_mask = 0x100\nprogress_value = 0x100\n\n",
        "[[step]]\naction = \"expect-live-update-marker-absent\"\n\n",
        "[[step]]\naction = \"expect-powered\"\npowered = false\nguest_timeout_ms = 4294967295\nwall_timeout_s = 30\n\n",
    ] {
        assert!(source.contains(missing));
        assert!(
            Scenario::parse(&source.replace(missing, "")).is_err(),
            "missing {missing}"
        );
    }
}

#[test]
fn l6_patched_update_requires_full_terminal_verification() {
    let source = include_str!("../../../../tests/emulator/cases/boot/l6-update-patched-midi.toml");
    assert!(Scenario::from_text(source).is_ok());
    for changed in [
        source.replace("profile = \"verify\"", "profile = \"restart\""),
        source.replace("guest_ms = 5000", "guest_ms = 1"),
        source.replace(
            "patches/midi-sync/tests/emulator/fragments/l6-hook-probe.toml",
            "tests/emulator/fragments/l6-update-install.toml",
        ),
        source.replace("models = [\"l6\"]", "models = [\"l6max\"]"),
        source.replace(
            "update_patches.l6 = [\"midi-sync\"]",
            "update_patches.l6 = []",
        ),
        source.replace(
            "action = \"lifecycle-stop\"",
            "action = \"delay\"\nguest_ms = 1",
        ),
    ] {
        assert!(Scenario::from_text(&changed).is_err());
    }
}

#[test]
fn synthetic_install_requires_all_boot_and_storage_checks() {
    let source =
        include_str!("../../../../tests/emulator/cases/boot/l6max-update-valid-install.toml");
    assert!(Scenario::from_text(source).is_ok());
    let first_installer = source
        .find("[[step]]\naction = \"lifecycle-start\"\nprofile = \"installer\"")
        .unwrap();
    assert!(Scenario::from_text(&source[..first_installer]).is_err());
    for missing in [
        "[[step]]\naction = \"expect-installer-outcome\"\nexpected = \"installed\"\n\n",
        "[[step]]\naction = \"wait-update-panel-ready\"\nrequire_programming = true\nguest_timeout_ms = 60000\nwall_timeout_s = 300\n\n",
        "[[step]]\naction = \"expect-update-comparison\"\nfield = \"protected-region\"\n\n",
        "[[step]]\naction = \"expect-installer-outcome\"\nexpected = \"no-pending\"\n\n",
        "[[step]]\naction = \"expect-panel-unchanged\"\n",
    ] {
        assert!(source.contains(missing));
        assert!(
            Scenario::from_text(&source.replace(missing, "")).is_err(),
            "missing {missing}"
        );
    }
    assert!(
        Scenario::from_text(&source.replace("profile = \"restart\"", "profile = \"installer\""))
            .is_err()
    );
}

#[test]
fn patched_update_requires_complete_four_boot_verification() {
    let source =
        include_str!("../../../../tests/emulator/cases/boot/l6max-update-patched-midi.toml");
    assert!(Scenario::from_text(source).is_ok());
    for missing in [
        "[[step]]\naction = \"end-wall-deadline\"\n\n",
        "[[step]]\naction = \"wait-usb-controller\"\nguest_timeout_ms = 180000\nwall_timeout_s = 300\n\n",
        "[[step]]\naction = \"direct-usb-midi-round-trip\"\nguest_timeout_ms = 180000\nwall_timeout_s = 300\n\n",
        "[[step]]\naction = \"include\"\npath = \"patches/midi-sync/tests/emulator/fragments/request-state.toml\"\ninclude_guest_timeout_ms = 180000\ninclude_wall_timeout_s = 300\n\n",
        "[[step]]\naction = \"expect-update-comparison\"\nfield = \"protected-region\"\n\n",
        "[[step]]\naction = \"expect-panel-unchanged\"\n",
    ] {
        assert!(source.contains(missing));
        assert!(
            Scenario::from_text(&source.replace(missing, "")).is_err(),
            "missing {missing}"
        );
    }
    assert!(
        Scenario::from_text(
            &source.replace("require_programming = false", "require_programming = true")
        )
        .is_err()
    );
    assert!(
        Scenario::from_text(&source.replace(
            "include_guest_timeout_ms = 180000",
            "include_guest_timeout_ms = 30000"
        ))
        .is_err()
    );
    assert!(
        Scenario::from_text(&source.replace(
            "update_patches.l6max = [\"midi-sync\"]",
            "update_patches.l6max = [\"knob-dialog\"]"
        ))
        .is_err()
    );
}

#[test]
fn l6_stock_update_requires_both_boots_and_all_comparisons() {
    let source = include_str!("../../../../tests/emulator/cases/boot/l6-update-stock-install.toml");
    assert!(Scenario::from_text(source).is_ok());
    for action in [
        "mark-l6-update-pending",
        "capture-update-marker-pending",
        "expect-installer-outcome",
        "expect-l6-update-reset-vector",
        "expect-update-marker-absent",
    ] {
        let needle = format!("action = \"{action}\"");
        assert!(
            Scenario::from_text(
                &source.replace(&needle, "action = \"checkpoint\"\nname = \"removed\"")
            )
            .is_err(),
            "{action}"
        );
    }
    for field in [
        "main-firmware",
        "main-trailer",
        "secondary-firmware",
        "secondary-trailer",
        "package-footer",
        "boot-region",
        "protected-region",
        "resource-nor",
    ] {
        let needle = format!("field = \"{field}\"");
        let replacement = if field == "resource-nor" {
            "field = \"boot-region\""
        } else {
            "field = \"resource-nor\""
        };
        assert!(
            Scenario::from_text(&source.replace(&needle, replacement)).is_err(),
            "{field}"
        );
    }
    assert!(Scenario::from_text(&source.replacen("guest_ms = 40000", "guest_ms = 1", 1)).is_err());
    assert!(
        Scenario::from_text(&source.replace("models = [\"l6\"]", "models = [\"l6max\"]")).is_err()
    );
    assert!(
        Scenario::from_text(&source.replace("profile = \"installer\"", "profile = \"restart\""))
            .is_err()
    );
}

#[test]
fn paced_turn_preserves_ring_baseline_and_exact_report_count() {
    let scenario = Scenario::parse("version=1\nname='paced'\nmodels=['l6max']\nbackends=['headless']\n[[step]]\naction='effect-paced-turn'\nchannel=1\nsteps=2\n[[step]]\naction='expect-ring'\nchannel=1\nmask=31\nonly=true\n").unwrap();
    let mut runner = Runner::new(
        scenario,
        Model::L6max,
        std::env::temp_dir().join(format!(
            "paced-unit-{}",
            NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )),
    )
    .unwrap();
    let now = Instant::now();
    assert!(matches!(
        runner.advance(sample(0), now).unwrap(),
        Progress::Device(crate::devices::Operation::EffectPacedTurn {
            channel: 1,
            steps: 2
        })
    ));
    runner.device_finished(serde_json::json!({"applied":2}));
    let mut extra = sample(200);
    extra.reports = 3;
    assert!(
        matches!(runner.advance(extra, now), Err(error) if error.contains("duplicate encoder reports"))
    );
    fs::remove_dir_all(&runner.logs).unwrap();
}
