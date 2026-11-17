#![allow(dead_code)]

#[path = "../l6max/src/math.rs"]
mod math;
#[path = "../l6max/src/plateau.rs"]
mod plateau;

#[cfg(test)]
mod regression_tests {
    use super::*;

    #[test]
    fn plateau_lowpass_has_minus_three_db_at_its_cutoff() {
        for cutoff in [20.0_f64, 1000.0, 6500.0, 16000.0] {
            let mut filter = plateau::OnePoleLP::new();
            filter.set_cutoff(cutoff as f32);
            let mut input_energy = 0.0;
            let mut output_energy = 0.0;
            for frame in 0..(48_000 + 16_384) {
                let phase = std::f64::consts::TAU * cutoff * frame as f64 / 48_000.0;
                let input = phase.sin() as f32;
                let output = filter.process(input);
                if frame >= 16_384 {
                    input_energy += (input as f64).powi(2);
                    output_energy += (output as f64).powi(2);
                }
            }
            let gain = (output_energy / input_energy).sqrt();
            assert!(
                (gain - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.015,
                "cutoff {cutoff} Hz has gain {gain}"
            );
        }
    }

    #[test]
    fn plateau_highpass_has_unity_passband_and_minus_three_db_cutoff() {
        for cutoff in [13.75_f64, 20.0, 1000.0, 6500.0, 16000.0, 23520.0] {
            let mut filter = plateau::OnePoleHP::new();
            filter.set_cutoff(cutoff as f32);
            let mut input_energy = 0.0;
            let mut output_energy = 0.0;
            for frame in 0..(48_000 + 16_384) {
                let phase = std::f64::consts::TAU * cutoff * frame as f64 / 48_000.0;
                let input = phase.sin() as f32;
                let output = filter.process(input);
                if frame >= 16_384 {
                    input_energy += (input as f64).powi(2);
                    output_energy += (output as f64).powi(2);
                }
            }
            let gain = (output_energy / input_energy).sqrt();
            assert!(
                (gain - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.015,
                "cutoff {cutoff} Hz has gain {gain}"
            );
            filter.clear();
            for frame in 0..10_000 {
                let input = if frame % 2 == 0 { 1.0 } else { -1.0 };
                let output = filter.process(input);
                if frame == 9_999 {
                    assert!(
                        (output.abs() - 1.0).abs() < 0.001,
                        "Nyquist passband at {cutoff} Hz has gain {}",
                        output.abs()
                    );
                }
            }
        }
    }

    #[test]
    fn plateau_cutoff_updates_preserve_independent_filter_histories() {
        let mut reverb = Box::new(plateau::PlateauReverb::new());
        reverb.init(plateau::PlateauParameters::default());
        let mut lp = [
            plateau::OnePoleLP::new(),
            plateau::OnePoleLP::new(),
            plateau::OnePoleLP::new(),
        ];
        let mut hp = [
            plateau::OnePoleHP::new(),
            plateau::OnePoleHP::new(),
            plateau::OnePoleHP::new(),
        ];
        lp[0].set_cutoff(14_080.0);
        hp[0].set_cutoff(13.75);
        for (low, high) in [(80.0, 6500.0), (900.0, 16000.0), (100.0, 1000.0)] {
            let mut params = reverb.params;
            params.low_cut_hz = low;
            params.high_cut_hz = high;
            reverb.set_parameters(params);
            for _ in 0..64 {
                reverb.process_sample(0.0, 0.0);
                hp[0].process(lp[0].process(0.0));
            }
            // Preserve each tank filter's post-transition history separately.
            lp[1] = reverb.l_lp;
            lp[2] = reverb.r_lp;
            hp[1] = reverb.l_hp;
            hp[2] = reverb.r_hp;
            for frame in 0..512 {
                let inputs = [
                    if frame % 37 == 0 { 0.5 } else { 0.0 },
                    if frame % 53 == 0 { -0.75 } else { 0.0 },
                    if frame % 71 == 0 { 1.0 } else { 0.0 },
                ];
                let actual_lp = [&mut reverb.in_lp, &mut reverb.l_lp, &mut reverb.r_lp];
                for (i, filter) in actual_lp.into_iter().enumerate() {
                    assert_eq!(filter.process(inputs[i]), lp[i].process(inputs[i]));
                }
                let actual_hp = [&mut reverb.in_hp, &mut reverb.l_hp, &mut reverb.r_hp];
                for (i, filter) in actual_hp.into_iter().enumerate() {
                    assert_eq!(filter.process(inputs[i]), hp[i].process(inputs[i]));
                }
            }
        }
    }

    #[test]
    fn plateau_damping_edit_ramps_and_retargets_without_a_coefficient_jump() {
        let mut reverb = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        params.high_cut_hz = 1000.0;
        reverb.init(params);
        let initial = reverb.l_lp.a;
        params.high_cut_hz = 16000.0;
        reverb.set_parameters(params);
        assert_eq!(
            reverb.l_lp.a, initial,
            "the setter must not jump the filter"
        );
        for _ in 0..20 {
            reverb.process_sample(0.0, 0.0);
        }
        let midway = reverb.l_lp.a;
        assert!(midway > initial && midway < 0.8);
        params.high_cut_hz = 1000.0;
        reverb.set_parameters(params);
        assert_eq!(
            reverb.l_lp.a, midway,
            "reversal must start from the applied coefficient"
        );
        for _ in 0..64 {
            reverb.process_sample(0.0, 0.0);
        }
        assert_eq!(reverb.l_lp.a, initial);
        assert_eq!(reverb.r_lp.a, initial);
        params.high_cut_hz = 16000.0;
        reverb.set_parameters(params);
        reverb.clear();
        let mut target = plateau::OnePoleLP::new();
        target.set_cutoff(16000.0);
        assert_eq!(
            reverb.l_lp.a, target.a,
            "clear must apply the requested cutoff"
        );
    }

    #[test]
    fn plateau_low_cut_ramps_without_changing_filter_history_at_the_edit() {
        let mut reverb = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        params.low_cut_hz = 20.0;
        reverb.init(params);
        let mut reference = plateau::OnePoleHP::new();
        reference.set_cutoff(20.0);
        assert_eq!(reverb.l_hp.process(0.5), reference.process(0.5));
        params.low_cut_hz = 1000.0;
        reverb.set_parameters(params);
        assert_eq!(reverb.l_hp.process(-0.2), reference.process(-0.2));
        for _ in 0..64 {
            reverb.process_sample(0.0, 0.0);
        }
        reverb.l_hp.clear();
        reference.clear();
        reference.set_cutoff(1000.0);
        assert_eq!(reverb.l_hp.process(0.5), reference.process(0.5));
    }

    #[test]
    fn plateau_zero_diffusion_bypasses_input_delays_and_keeps_histories_current() {
        let mut reverb = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        params.predelay_ms = 0.0;
        params.diffusion = 0.0;
        reverb.init(params);
        reverb.process_sample(0.5, 0.5);
        assert!(
            reverb.l_apf1.delay.buffer[0].abs() > 0.01
                && reverb.r_apf1.delay.buffer[0].abs() > 0.01,
            "zero diffusion should reach both tanks without the input delay chain"
        );
        for _ in 0..2500 {
            reverb.process_sample(0.0, 0.0);
        }
        assert!(
            reverb.in_apf4.delay.buffer.iter().any(|x| *x != 0.0),
            "bypassed diffusers must continue receiving input"
        );

        params.diffusion = 0.5;
        reverb.set_parameters(params);
        for _ in 0..96 {
            let (l, r) = reverb.process_sample(0.0, 0.0);
            assert!(l.is_finite() && r.is_finite() && l.abs() <= 1.0 && r.abs() <= 1.0);
        }
    }

    #[test]
    fn plateau_diffusion_edit_avoids_first_frame_jump() {
        let mut changed = Box::new(plateau::PlateauReverb::new());
        let mut unchanged = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        params.predelay_ms = 0.0;
        changed.init(params);
        unchanged.init(params);
        let input = |frame: usize| ((frame * 7919 % 127) as f32 - 63.0) / 90.0;
        for frame in 0..10_000 {
            let sample = input(frame);
            assert_eq!(
                changed.process_sample(sample, sample),
                unchanged.process_sample(sample, sample)
            );
        }
        params.diffusion = 0.0;
        changed.set_parameters(params);
        let sample = input(10_000);
        let actual = changed.process_sample(sample, sample);
        let reference = unchanged.process_sample(sample, sample);
        let difference = (actual.0 - reference.0)
            .abs()
            .max((actual.1 - reference.1).abs());
        assert!(difference < 0.002, "first-frame change {difference}");
    }

    #[test]
    fn plateau_diffusion_edits_ramp_allpass_gains_and_retarget() {
        let mut reverb = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        reverb.init(params);
        let initial = reverb.l_apf2.gain;
        assert_eq!(initial, 0.7 * params.diffusion);
        assert_eq!(reverb.l_apf1.gain, -initial);
        assert_eq!(reverb.r_apf1.gain, -initial);
        assert_eq!(reverb.r_apf2.gain, initial);
        assert_eq!(
            [
                reverb.in_apf1.gain,
                reverb.in_apf2.gain,
                reverb.in_apf3.gain,
                reverb.in_apf4.gain
            ],
            [0.75, 0.75, 0.625, 0.625]
        );
        params.diffusion = 0.0;
        reverb.set_parameters(params);
        assert_eq!(
            reverb.l_apf2.gain, initial,
            "the setter must not jump the coefficient"
        );
        for _ in 0..20 {
            reverb.process_sample(0.0, 0.0);
        }
        let halfway = reverb.l_apf2.gain;
        assert!(halfway > 0.0 && halfway < initial);
        params.diffusion = 1.0;
        reverb.set_parameters(params);
        assert_eq!(
            reverb.l_apf2.gain, halfway,
            "reversal must start at the applied gain"
        );
        for _ in 0..256 {
            reverb.process_sample(0.0, 0.0);
        }
        assert_eq!(
            [
                reverb.l_apf1.gain,
                reverb.l_apf2.gain,
                reverb.r_apf1.gain,
                reverb.r_apf2.gain
            ],
            [-0.7, 0.7, -0.7, 0.7]
        );
        assert_eq!(
            [
                reverb.in_apf1.gain,
                reverb.in_apf2.gain,
                reverb.in_apf3.gain,
                reverb.in_apf4.gain
            ],
            [0.75, 0.75, 0.625, 0.625]
        );
        params.diffusion = 0.0;
        reverb.set_parameters(params);
        reverb.clear();
        assert_eq!(
            [
                reverb.l_apf1.gain,
                reverb.l_apf2.gain,
                reverb.r_apf1.gain,
                reverb.r_apf2.gain
            ],
            [0.0; 4],
            "clear should apply the latest setting"
        );
    }

    #[test]
    fn plateau_diffusion_changes_tank_response_after_input_diffusers() {
        let mut dry_tank = Box::new(plateau::PlateauReverb::new());
        let mut diffused_tank = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        params.predelay_ms = 0.0;
        params.diffusion = 1.0;
        dry_tank.init(params);
        diffused_tank.init(params);
        // Isolate the tank: keep both input-diffuser paths enabled and identical.
        params.diffusion = 0.5;
        dry_tank.set_parameters(params);
        for _ in 0..64 {
            dry_tank.process_sample(0.0, 0.0);
            diffused_tank.process_sample(0.0, 0.0);
        }
        let mut difference = 0.0f32;
        for frame in 0..32_000 {
            let input = if frame == 0 { 0.5 } else { 0.0 };
            let a = dry_tank.process_sample(input, input);
            let b = diffused_tank.process_sample(input, input);
            difference = difference.max((a.0 - b.0).abs()).max((a.1 - b.1).abs());
        }
        assert!(
            difference > 0.001,
            "tank diffusion did not change the impulse response: {difference}"
        );
    }

    #[test]
    fn plateau_output_taps_use_full_history_independent_of_size() {
        let mut reverb = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        params.size = 0.2;
        params.freeze = true;
        reverb.init(params);

        // Plateau's seven tap offsets and signs, in 29,761 Hz samples.
        // The first value names the retained line (L/R delay 1, allpass 2,
        // or delay 2); the final value selects the output channel.
        let cases = [
            (0, 266.0f32, 1.0, false),
            (0, 2974.0, 1.0, false),
            (1, 1913.0, -1.0, false),
            (2, 1996.0, 1.0, false),
            (3, 1990.0, -1.0, false),
            (4, 187.0, -1.0, false),
            (5, 1066.0, -1.0, false),
            (3, 266.0, 1.0, true),
            (3, 2974.0, 1.0, true),
            (4, 1913.0, -1.0, true),
            (5, 1996.0, 1.0, true),
            (0, 1990.0, -1.0, true),
            (1, 187.0, -1.0, true),
            (2, 1066.0, -1.0, true),
        ];
        assert!((2974.0f32 * (48_000.0 / 29_761.0)) as usize > reverb.r_del1.length);
        for (line, offset, sign, right_channel) in cases {
            reverb.clear();
            let history: &mut [f32] = match line {
                0 => &mut reverb.l_del1.buffer,
                1 => &mut reverb.l_apf2.delay.buffer,
                2 => &mut reverb.l_del2.buffer,
                3 => &mut reverb.r_del1.buffer,
                4 => &mut reverb.r_apf2.delay.buffer,
                5 => &mut reverb.r_del2.buffer,
                _ => unreachable!(),
            };
            let tap = (offset * (48_000.0 / 29_761.0)) as usize;
            assert!(tap < history.len());
            let index = (1 + history.len() - tap) % history.len();
            history[index] = 0.5;
            let (left, right) = reverb.process_sample(0.0, 0.0);
            let mut dc = plateau::OnePoleHP::new();
            dc.set_cutoff(20.0);
            let expected = math::soft_clip(dc.process(sign * 0.5) * 0.5);
            let (actual, other) = if right_channel {
                (right, left)
            } else {
                (left, right)
            };
            assert!(
                other.abs() < 1e-7,
                "line {line}, tap {tap}: opposite channel {other}"
            );
            assert!(
                (actual - expected).abs() < 1e-6,
                "line {line}, tap {tap}: actual {actual}, expected {expected}"
            );
        }
    }

    #[test]
    fn plateau_first_tank_allpass_reaches_each_output() {
        let mut reverb = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        params.diffusion = 1.0;
        reverb.init(params);
        reverb.params.freeze = true;
        reverb.l_sum = 0.5;
        reverb.r_sum = -0.25;

        // PlateauNEVersio Dattorro.cpp, commit 4218b072e7420dd788cd3c98c36b3eba445f8886,
        // includes each first tank-allpass output in its matching channel.
        // All other tank taps start silent. With gain -0.7, the first
        // allpasses immediately contribute 0.35 and -0.175 respectively.
        let mut left_hp = plateau::OnePoleHP::new();
        let mut right_hp = plateau::OnePoleHP::new();
        left_hp.set_cutoff(20.0);
        right_hp.set_cutoff(20.0);
        let wet = reverb.params.wet;
        let expected = (
            math::soft_clip(left_hp.process(0.35) * 0.5) * wet,
            math::soft_clip(right_hp.process(-0.175) * 0.5) * wet,
        );
        let actual = reverb.process_sample(0.0, 0.0);
        assert!(
            (actual.0 - expected.0).abs() < 1e-6,
            "left: {actual:?} vs {expected:?}"
        );
        assert!(
            (actual.1 - expected.1).abs() < 1e-6,
            "right: {actual:?} vs {expected:?}"
        );
    }

    #[test]
    fn plateau_tank_delays_match_their_configured_ring_lengths() {
        let mut reverb = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        params.diffusion = 1.0;
        reverb.init(params);
        reverb.params.freeze = true;
        reverb.l_del1.length = 10;
        reverb.l_del2.length = 10;
        reverb.r_del1.length = 10;
        reverb.r_del2.length = 10;
        reverb.l_sum = 1.0;
        reverb.r_sum = 1.0;

        // With empty allpasses the first tank allpass produces an immediate
        // 0.7 impulse, then remains silent past these short test rings.
        for frame in 0..=20 {
            reverb.process_sample(0.0, 0.0);
            let l = reverb.l_apf2.delay.buffer[frame];
            let r = reverb.r_apf2.delay.buffer[frame];
            let expected = if frame == 10 { 0.7 } else { 0.0 };
            assert!(
                (l - expected).abs() < 1e-6,
                "left first tank delay frame {frame}: {l}"
            );
            assert!(
                (r - expected).abs() < 1e-6,
                "right first tank delay frame {frame}: {r}"
            );
            let feedback = if frame == 20 { -0.49 } else { 0.0 };
            assert!(
                (reverb.l_sum - feedback).abs() < 1e-6,
                "left cross-feedback frame {frame}: {}",
                reverb.l_sum
            );
            assert!(
                (reverb.r_sum - feedback).abs() < 1e-6,
                "right cross-feedback frame {frame}: {}",
                reverb.r_sum
            );
        }
    }

    #[test]
    fn plateau_predelay_edits_crossfade_and_retarget_without_a_step() {
        let mut reverb = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        params.predelay_ms = 0.0;
        reverb.init(params);
        // The first allpass cannot feed back within this short test. Its last
        // written input is therefore the output of the predelay.
        let diffuser_input = |r: &plateau::PlateauReverb| {
            let d = &r.in_apf1.delay;
            d.buffer[(d.write_pos + d.buffer.len() - 1) % d.buffer.len()]
        };
        reverb.in_lp.a = 1.0;
        reverb.in_lp.b = 0.0;
        reverb.predelay.buffer.fill(-1.0);
        let mut dc = plateau::OnePoleHP::new();
        dc.set_cutoff(20.0);
        let mut hp = plateau::OnePoleHP::new();
        hp.set_cutoff(13.75);
        reverb.process_sample(1.0, 1.0);
        assert_eq!(diffuser_input(&reverb), hp.process(dc.process(1.0)));

        reverb.set_predelay_live(10.0);
        reverb.process_sample(1.0, 1.0);
        let filtered = hp.process(dc.process(1.0));
        assert!((diffuser_input(&reverb) - (filtered + (-1.0 - filtered) / 64.0)).abs() < 1e-6);
        for _ in 0..15 {
            reverb.process_sample(1.0, 1.0);
            hp.process(dc.process(1.0));
        }
        let before_reverse = diffuser_input(&reverb);
        reverb.set_predelay_live(0.0);
        reverb.process_sample(1.0, 1.0);
        let filtered = hp.process(dc.process(1.0));
        let after_reverse = diffuser_input(&reverb);
        assert!(
            (after_reverse - (before_reverse + (filtered - before_reverse) / 64.0)).abs() < 1e-6
        );
        assert!(after_reverse >= before_reverse);
        assert!((after_reverse - before_reverse).abs() <= 2.0 / 64.0);
        let mut filtered = 0.0;
        for _ in 0..63 {
            reverb.process_sample(1.0, 1.0);
            filtered = hp.process(dc.process(1.0));
        }
        assert_eq!(diffuser_input(&reverb), filtered);
    }

    #[test]
    fn plateau_predelay_edit_during_freeze_does_not_replay_frozen_input() {
        let mut reverb = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        params.predelay_ms = 0.0;
        reverb.init(params);
        reverb.in_lp.a = 1.0;
        reverb.in_lp.b = 0.0;
        reverb.set_predelay_live(190.0);
        for _ in 0..8 {
            reverb.process_sample(1.0, 1.0);
        }
        params = reverb.params;
        params.freeze = true;
        reverb.set_live_parameters(params);
        reverb.set_predelay_live(0.0);
        for _ in 0..8 {
            reverb.process_sample(1.0, 1.0);
        }
        params = reverb.params;
        params.freeze = false;
        reverb.set_live_parameters(params);
        reverb.process_sample(0.0, 0.0);
        assert_eq!(reverb.in_lp.z, 0.0);
    }

    #[test]
    fn shrinking_plateau_size_keeps_new_samples_in_the_delay_ring() {
        let mut reverb = Box::new(plateau::PlateauReverb::new());
        reverb.init(plateau::PlateauParameters::default());
        // These are valid positions just before each old ring wraps.
        reverb.l_del1.write_pos = reverb.l_del1.length - 1;
        reverb.l_del2.write_pos = reverb.l_del2.length - 1;
        reverb.r_del1.write_pos = reverb.r_del1.length - 1;
        reverb.r_del2.write_pos = reverb.r_del2.length - 1;
        let mut params = reverb.params;
        params.size = 0.2;
        reverb.set_parameters(params);
        reverb.l_del1.write(0.75);
        reverb.l_del2.write(0.75);
        reverb.r_del1.write(0.75);
        reverb.r_del2.write(0.75);
        assert_eq!(reverb.l_del1.read(1), 0.75);
        assert_eq!(reverb.l_del2.read(1), 0.75);
        assert_eq!(reverb.r_del1.read(1), 0.75);
        assert_eq!(reverb.r_del2.read(1), 0.75);
    }

    #[test]
    fn plateau_size_expansion_does_not_replay_abandoned_history() {
        let mut reverb = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        params.size = 1.2;
        params.decay = 0.1;
        reverb.init(params);

        // Fill the long tank delays, then let the short reverb decay well
        // beyond the largest backing buffer's duration.
        for frame in 0..96_000 {
            let input = if frame % 100 < 50 { 0.7 } else { -0.7 };
            reverb.process_sample(input, input);
        }
        params.size = 0.2;
        reverb.set_parameters(params);
        for _ in 0..144_000 {
            reverb.process_sample(0.0, 0.0);
        }
        params.size = 1.2;
        reverb.set_parameters(params);
        let mut peak = 0.0f32;
        for _ in 0..16_000 {
            let (left, right) = reverb.process_sample(0.0, 0.0);
            peak = peak.max(left.abs()).max(right.abs());
        }
        assert!(peak < 1e-5, "size expansion replayed an old tail: {peak}");

        // Discarding stale history must not prevent subsequent input from
        // reaching the reverb.
        let mut response = 0.0f32;
        for frame in 0..16_000 {
            let input = if frame == 0 { 1.0 } else { 0.0 };
            let (left, right) = reverb.process_sample(input, input);
            response = response.max(left.abs()).max(right.abs());
        }
        assert!(response > 0.001, "reverb did not respond to new input");
    }

    #[test]
    fn plateau_maximum_settings_fit_delay_histories_and_output_taps() {
        let mut reverb = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        params.size = 1.2;
        params.predelay_ms = 190.0;
        params.mod_depth = 16.0;
        reverb.init(params);
        let scale = (48_000.0f32 / 29_761.0) * params.size;
        let samples = |value: f32| (value * scale) as usize;
        let input_samples = |value: f32| (value * (48_000.0f32 / 29_761.0)) as usize;
        assert_eq!(reverb.predelay_samples, 9_120);
        assert!(reverb.predelay_samples < reverb.predelay.buffer.len());
        for (actual, expected) in [
            (reverb.in_apf1.delay_time, input_samples(141.0)),
            (reverb.in_apf2.delay_time, input_samples(107.0)),
            (reverb.in_apf3.delay_time, input_samples(379.0)),
            (reverb.in_apf4.delay_time, input_samples(277.0)),
            (reverb.l_apf1.delay_time, samples(672.0)),
            (reverb.l_apf2.delay_time, samples(1800.0)),
            (reverb.r_apf1.delay_time, samples(908.0)),
            (reverb.r_apf2.delay_time, samples(2656.0)),
            (reverb.l_del1.length, samples(4453.0)),
            (reverb.l_del2.length, samples(3720.0)),
            (reverb.r_del1.length, samples(4217.0)),
            (reverb.r_del2.length, samples(3163.0)),
        ] {
            assert_eq!(actual, expected, "maximum delay was clamped");
        }
        for (delay, capacity) in [
            (reverb.l_apf1.delay_time, reverb.l_apf1.delay.buffer.len()),
            (reverb.l_apf2.delay_time, reverb.l_apf2.delay.buffer.len()),
            (reverb.r_apf1.delay_time, reverb.r_apf1.delay.buffer.len()),
            (reverb.r_apf2.delay_time, reverb.r_apf2.delay.buffer.len()),
        ] {
            assert!(delay + params.mod_depth as usize <= capacity - 2);
        }
        for (tap, capacity) in [
            (2974.0, reverb.l_del1.buffer.len()),
            (1913.0, reverb.l_apf2.delay.buffer.len()),
            (1996.0, reverb.l_del2.buffer.len()),
            (2974.0, reverb.r_del1.buffer.len()),
            (1913.0, reverb.r_apf2.delay.buffer.len()),
            (1996.0, reverb.r_del2.buffer.len()),
        ] {
            let tap_samples = (tap * (48_000.0f32 / 29_761.0)) as usize;
            assert!(
                tap_samples < capacity,
                "output tap exceeded retained history"
            );
        }
        params.size = 0.2;
        reverb.set_parameters(params);
        for delay in [
            reverb.l_apf1.delay_time,
            reverb.l_apf2.delay_time,
            reverb.r_apf1.delay_time,
            reverb.r_apf2.delay_time,
        ] {
            assert!(
                delay >= params.mod_depth as usize,
                "size 0.2 would clamp modulation"
            );
        }
    }

    #[test]
    fn plateau_size_changes_tank_without_retiming_input_diffusers() {
        let mut reverb = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        reverb.init(params);
        let initial = [
            reverb.in_apf1.delay_time,
            reverb.in_apf2.delay_time,
            reverb.in_apf3.delay_time,
            reverb.in_apf4.delay_time,
        ];
        let expected =
            [141.0f32, 107.0, 379.0, 277.0].map(|time| (time * (48_000.0f32 / 29_761.0)) as usize);
        assert_eq!(initial, expected);
        reverb.in_apf1.delay.write(0.75);
        for size in [0.01, 0.2, 0.55, 1.0, 1.2] {
            params.size = size;
            reverb.set_parameters(params);
            assert_eq!(
                [
                    reverb.in_apf1.delay_time,
                    reverb.in_apf2.delay_time,
                    reverb.in_apf3.delay_time,
                    reverb.in_apf4.delay_time
                ],
                initial
            );
            assert_eq!(reverb.in_apf1.delay.read(1), 0.75);
            let expected_tank = (4453.0f32 * ((48_000.0f32 / 29_761.0) * size)) as usize;
            assert_eq!(reverb.l_del1.length, expected_tank, "size {size}");
        }
    }

    #[test]
    fn plateau_live_size_edit_transitions_without_an_output_step() {
        let mut reverb = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        reverb.init(params);
        let mut previous = (0.0f32, 0.0f32);
        let mut max_step = 0.0f32;
        for frame in 0..96_064 {
            if frame == 96_000 {
                params.size += 0.02;
                reverb.set_parameters(params);
            }
            let input = (frame as f32 * std::f32::consts::TAU * 250.0 / 48_000.0).sin() * 0.1;
            let output = reverb.process_sample(input, input);
            if frame >= 96_000 {
                max_step = max_step
                    .max((output.0 - previous.0).abs())
                    .max((output.1 - previous.1).abs());
            }
            previous = output;
        }
        assert!(
            max_step < 0.04,
            "size edit produced a large output step: {max_step}"
        );

        for frame in 0..64 {
            let editing = frame % 8 == 0;
            if editing {
                params.size = if params.size < 1.01 { 1.02 } else { 1.0 };
                reverb.set_parameters(params);
            }
            let output = reverb.process_sample(0.1, 0.1);
            if editing {
                assert!((output.0 - previous.0).abs() <= 0.032);
                assert!((output.1 - previous.1).abs() <= 0.032);
            }
            previous = output;
        }

        // The wet control must take effect immediately, including when a
        // further size edit restarts an in-progress transition.
        params.size -= 0.02;
        params.wet = 0.0;
        reverb.set_parameters(params);
        for frame in 0..16 {
            if frame == 8 {
                params.size += 0.02;
                reverb.set_parameters(params);
            }
            assert_eq!(reverb.process_sample(0.1, 0.1), (0.0, 0.0));
        }
    }

    #[test]
    fn test_plateau_reverb_decay() {
        let mut plat = plateau::PlateauReverb::new();
        plat.init(plateau::PlateauParameters::default());

        // Feed impulse
        let (y_l, y_r) = plat.process_sample(1.0, 1.0);
        let mut max_tail = y_l.abs().max(y_r.abs());

        // Process 1 second of tail
        for _ in 1..48000 {
            let (l, r) = plat.process_sample(0.0, 0.0);
            assert!(l.is_finite(), "Reverb L not finite");
            assert!(r.is_finite(), "Reverb R not finite");
            max_tail = max_tail.max(l.abs()).max(r.abs());
        }

        assert!(max_tail > 0.01, "Reverb did not produce output");

        // After 2 seconds, tail should have decayed
        let (final_l, final_r) = plat.process_sample(0.0, 0.0);
        assert!(final_l.abs() < 0.01, "Reverb did not decay: {final_l}");
        assert!(final_r.abs() < 0.01, "Reverb did not decay: {final_r}");
    }

    #[test]
    fn test_plateau_freeze() {
        let mut plat = plateau::PlateauReverb::new();
        let mut params = plateau::PlateauParameters::default();
        plat.init(params);

        // Fill the tank before freezing; a short input mostly exercises its
        // initial transient rather than sustained feedback.
        for i in 0..96_000 {
            let s = ((i as f32) * 0.057595866).sin() * 0.25;
            plat.process_sample(s, s);
        }

        // Turn on Freeze
        params.freeze = true;
        plat.set_parameters(params);

        // Compare successive five-second windows rather than relying on one
        // short energy measurement, and check the output remains bounded.
        let mut energy = [0.0f64; 2];
        for window in &mut energy {
            for _ in 0..240_000 {
                let (l, r) = plat.process_sample(0.0, 0.0);
                assert!(l.is_finite() && r.is_finite());
                assert!(l.abs() <= 1.0 && r.abs() <= 1.0);
                *window += (l as f64) * (l as f64) + (r as f64) * (r as f64);
            }
        }
        assert!(energy[0] > 1.0, "frozen tank was not energized");
        assert!(
            energy[1] > energy[0] * 0.5 && energy[1] < energy[0] * 2.0,
            "frozen energy changed excessively: {energy:?}"
        );
    }

    #[test]
    fn plateau_modulates_both_second_tank_allpasses() {
        let mut first = Box::new(plateau::PlateauReverb::new());
        let mut second = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        params.mod_depth = 16.0;
        first.init(params);
        second.init(params);
        for engine in [&mut first, &mut second] {
            for (index, sample) in engine.l_apf2.delay.buffer.iter_mut().enumerate() {
                *sample = index as f32 / 5000.0;
            }
            for (index, sample) in engine.r_apf2.delay.buffer.iter_mut().enumerate() {
                *sample = index as f32 / 6000.0;
            }
        }
        first.lfo2.phase = 0.1;
        second.lfo2.phase = 0.6;
        first.lfo4.phase = 0.2;
        second.lfo4.phase = 0.7;
        first.process_sample(0.0, 0.0);
        second.process_sample(0.0, 0.0);
        // Each allpass consumes the preceding frame's interpolated delay read.
        first.process_sample(0.0, 0.0);
        second.process_sample(0.0, 0.0);
        assert_ne!(
            first.l_del2.buffer[1], second.l_del2.buffer[1],
            "left second allpass"
        );
        assert_ne!(
            first.r_del2.buffer[1], second.r_del2.buffer[1],
            "right second allpass"
        );
    }

    #[test]
    fn plateau_freeze_preserves_tank_energy_with_maximum_modulation() {
        for size in [0.01, 0.2, 0.6, 1.0, 1.2] {
            let mut reverb = Box::new(plateau::PlateauReverb::new());
            let mut params = plateau::PlateauParameters::default();
            params.size = size;
            params.decay = 0.999;
            params.mod_depth = 16.0;
            reverb.init(params);
            let mut random = 7u32;
            for _ in 0..240_000 {
                random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let input = random as i32 as f32 * (0.25 / 2_147_483_648.0);
                reverb.process_sample(input, input);
            }
            params.freeze = true;
            reverb.set_parameters(params);
            let mut energy = [0.0f64; 2];
            for window in &mut energy {
                for _ in 0..960_000 {
                    reverb.process_sample(0.0, 0.0);
                    let (l, r) = (reverb.l_sum as f64, reverb.r_sum as f64);
                    *window += l * l + r * r;
                }
            }
            assert!(energy[0] > 1.0, "tank must be energized for size {size}");
            assert!(
                energy[1] > energy[0] * 0.85 && energy[1] < energy[0] * 1.15,
                "frozen tank energy changed excessively for size {size}: {energy:?}"
            );
        }
    }

    #[test]
    fn plateau_freeze_holds_modulation_then_resumes_it() {
        let mut modulated = Box::new(plateau::PlateauReverb::new());
        let mut fixed = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        params.mod_depth = 16.0;
        modulated.init(params);
        fixed.init(params);
        for frame in 0..48_000 {
            let input = (frame as f32 * 0.031).sin() * 0.2;
            assert_eq!(
                modulated.process_sample(input, input),
                fixed.process_sample(input, input)
            );
        }
        params.freeze = true;
        modulated.set_parameters(params);
        params.mod_depth = 0.0;
        fixed.set_parameters(params);
        let phases_before = [
            modulated.lfo1.phase,
            modulated.lfo2.phase,
            modulated.lfo3.phase,
            modulated.lfo4.phase,
        ];
        for frame in 0..20_000 {
            assert_eq!(
                modulated.process_sample(0.0, 0.0),
                fixed.process_sample(0.0, 0.0),
                "modulation affected frozen output at frame {frame}"
            );
        }
        for (before, after) in phases_before.into_iter().zip([
            modulated.lfo1.phase,
            modulated.lfo2.phase,
            modulated.lfo3.phase,
            modulated.lfo4.phase,
        ]) {
            assert_ne!(after, before, "each LFO must continue advancing");
        }
        params.freeze = false;
        fixed.set_parameters(params);
        params.mod_depth = 16.0;
        modulated.set_parameters(params);
        let mut difference = 0.0f32;
        for frame in 0..20_000 {
            let input = (frame as f32 * 0.043).sin() * 0.1;
            let a = modulated.process_sample(input, input);
            let b = fixed.process_sample(input, input);
            difference = difference.max((a.0 - b.0).abs()).max((a.1 - b.1).abs());
        }
        assert!(difference > 1e-4, "modulation did not resume after freeze");
    }

    #[test]
    fn plateau_freeze_blocks_buffered_diffuser_signal_from_tank() {
        let mut reverb = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        reverb.init(params);
        // Leave an impulse in the final diffuser while the tank is empty.
        reverb.in_apf4.process(1.0);
        params.freeze = true;
        reverb.set_parameters(params);
        for frame in 0..12_000 {
            let (left, right) = reverb.process_sample(0.0, 0.0);
            assert_eq!((left, right), (0.0, 0.0), "frame {frame}");
        }
        params.freeze = false;
        reverb.set_parameters(params);
        let mut peak = 0.0f32;
        for frame in 0..12_000 {
            let input = if frame == 0 { 1.0 } else { 0.0 };
            let (left, right) = reverb.process_sample(input, input);
            peak = peak.max(left.abs()).max(right.abs());
        }
        assert!(peak > 0.001, "input must reach the tank after unfreezing");
    }

    #[test]
    fn plateau_freeze_does_not_replay_old_damping_state() {
        let mut reverb = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        reverb.init(params);
        // Model filter history from before freeze with an otherwise empty tank.
        reverb.l_hp.process(1.0);
        reverb.r_hp.process(-1.0);
        params.freeze = true;
        reverb.set_parameters(params);
        for _ in 0..20_000 {
            assert_eq!(reverb.process_sample(0.0, 0.0), (0.0, 0.0));
        }
        params.freeze = false;
        reverb.set_parameters(params);
        let mut peak = 0.0f32;
        for _ in 0..12_000 {
            let (left, right) = reverb.process_sample(0.0, 0.0);
            peak = peak.max(left.abs()).max(right.abs());
        }
        assert!(
            peak < 1e-7,
            "stale damping state reappeared after freeze: {peak}"
        );
    }

    #[test]
    fn plateau_input_dc_filter_precedes_predelay_and_rejects_frozen_input() {
        let mut reverb = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        reverb.init(params);
        let mut left = plateau::OnePoleHP::new();
        let mut right = plateau::OnePoleHP::new();
        left.set_cutoff(20.0);
        right.set_cutoff(20.0);
        let mut lp = plateau::OnePoleLP::new();
        lp.set_cutoff(14_080.0);
        let mut hp = plateau::OnePoleHP::new();
        hp.set_cutoff(13.75);

        for frame in 0..512 {
            if frame == 160 {
                params.freeze = true;
                reverb.set_parameters(params);
                left.clear();
                right.clear();
                lp.clear();
                hp.clear();
            }
            if frame == 320 {
                params.freeze = false;
                reverb.set_parameters(params);
            }
            let l = if frame % 17 == 0 { 0.75 } else { -0.1 };
            let r = if frame % 23 == 0 { -0.5 } else { 0.2 };
            let (input_l, input_r) = if params.freeze { (0.0, 0.0) } else { (l, r) };
            let expected =
                hp.process(lp.process(0.5 * (left.process(input_l) + right.process(input_r))));
            let pos = reverb.predelay.write_pos;
            reverb.process_sample(l, r);
            let expected = if params.freeze { 0.0 } else { expected };
            assert!(
                (reverb.predelay.buffer[pos] - expected).abs() < 1e-6,
                "frame {frame}"
            );
        }
        reverb.clear();
        let pos = reverb.predelay.write_pos;
        reverb.process_sample(1.0, 0.0);
        let mut fresh = plateau::OnePoleHP::new();
        fresh.set_cutoff(20.0);
        lp.clear();
        hp.clear();
        assert_eq!(
            reverb.predelay.buffer[pos],
            hp.process(lp.process(fresh.process(0.5)))
        );
    }

    #[test]
    fn plateau_freeze_does_not_delay_frozen_input_until_release() {
        let mut reverb = Box::new(plateau::PlateauReverb::new());
        let mut params = plateau::PlateauParameters::default();
        params.predelay_ms = 100.0;
        params.freeze = true;
        reverb.init(params);
        for frame in 0..64 {
            let input = if frame == 63 { 1.0 } else { 0.0 };
            assert_eq!(reverb.process_sample(input, input), (0.0, 0.0));
        }
        params.freeze = false;
        reverb.set_parameters(params);
        for frame in 0..12_000 {
            let (left, right) = reverb.process_sample(0.0, 0.0);
            assert_eq!(
                (left, right),
                (0.0, 0.0),
                "frozen input leaked at frame {frame}"
            );
        }
        let mut peak = 0.0f32;
        for frame in 0..16_000 {
            let input = if frame == 0 { 1.0 } else { 0.0 };
            let (left, right) = reverb.process_sample(input, input);
            peak = peak.max(left.abs()).max(right.abs());
        }
        assert!(peak > 0.001, "new input must pass after freeze is released");
    }

    #[test]
    fn nonfinite_plateau_input_does_not_poison_effect_state() {
        let mut actual = plateau::PlateauReverb::new();
        let mut expected = plateau::PlateauReverb::new();
        actual.init(plateau::PlateauParameters::default());
        expected.init(plateau::PlateauParameters::default());
        for frame in 0..12_000 {
            let signal = if frame == 10 { 0.5 } else { 0.0 };
            let input = match frame {
                0 => f32::NAN,
                1 => f32::INFINITY,
                _ => signal,
            };
            assert_eq!(
                actual.process_sample(input, input),
                expected.process_sample(signal, signal),
                "plateau frame {frame}"
            );
        }
        assert_eq!(math::clamp(f32::NAN, 0.0, 1.0), 0.0);
        assert_eq!(math::soft_clip(f32::NAN), 0.0);
    }

    #[test]
    fn extreme_finite_plateau_input_does_not_poison_effect_state() {
        let mut actual = plateau::PlateauReverb::new();
        let mut expected = plateau::PlateauReverb::new();
        actual.init(plateau::PlateauParameters::default());
        expected.init(plateau::PlateauParameters::default());
        for frame in 0..12_000 {
            let signal = if frame == 10 { 0.5 } else { 0.0 };
            let input = match frame {
                0 => f32::MAX,
                1 => f32::MIN,
                _ => signal,
            };
            assert_eq!(
                actual.process_sample(input, input),
                expected.process_sample(signal, signal),
                "plateau frame {frame}"
            );
        }
    }
}
