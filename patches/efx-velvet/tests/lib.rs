#![allow(dead_code)]

#[path = "../../efx-loader/l6max/src/math.rs"]
mod math;
#[path = "../l6max/src/velvet.rs"]
mod velvet;

#[cfg(test)]
mod regression_tests {
    use super::*;

    #[test]
    fn test_math_functions() {
        assert!((math::sin(0.0)).abs() < 1e-4);
        assert!((math::sin(math::PI_2) - 1.0).abs() < 1e-3);
        assert!((math::cos(0.0) - 1.0).abs() < 1e-3);
        assert!((math::cos(math::PI_2)).abs() < 1e-3);

        assert!((math::sqrt(4.0) - 2.0).abs() < 1e-4);
        assert!((math::sqrt(100.0) - 10.0).abs() < 1e-4);

        assert!((math::pow10(0.0) - 1.0).abs() < 1e-3);
        assert!((math::pow10(1.0) - 10.0).abs() < 0.1);
        assert!((math::db_to_gain(0.0) - 1.0).abs() < 1e-3);
        assert!((math::db_to_gain(-20.0) - 0.1).abs() < 0.01);
    }

    #[test]
    fn velvet_uniform_matches_double_precision_reference() {
        let mut actual_state = 0x91a2_b3c4_d5e6_f708;
        let mut reference_state = actual_state;
        for _ in 0..200_000 {
            let actual = velvet::uniform_signed(&mut actual_state);
            let bits = velvet::splitmix64(&mut reference_state) >> 11;
            let expected = (bits as f64 * (1.0 / 9007199254740992.0) * 2.0 - 1.0) as f32;
            assert_eq!(actual.to_bits(), expected.to_bits());
        }
        // Exercise rounding boundaries and both endpoints of the 53-bit domain.
        for exponent in 0..53 {
            let base = 1u64 << exponent;
            for bits in [base - 1, base, base + 1, (1u64 << 53) - base] {
                // Invert SplitMix64 so this case exercises uniform_signed itself.
                let mut state = bits << 11;
                state ^= state >> 31 ^ state >> 62;
                state = state.wrapping_mul(0x3196_42b2_d24d_8ec3);
                state ^= state >> 27 ^ state >> 54;
                state = state.wrapping_mul(0x96de_1b17_3f11_9089);
                state ^= state >> 30 ^ state >> 60;
                state = state.wrapping_sub(0x9e37_79b9_7f4a_7c15);
                let expected = (bits as f64 * (1.0 / 9007199254740992.0) * 2.0 - 1.0) as f32;
                let actual = velvet::uniform_signed(&mut state);
                assert_eq!(actual.to_bits(), expected.to_bits(), "bits={bits}");
            }
        }
    }

    #[test]
    fn sine_range_reduction_terminates_for_extreme_inputs() {
        for x in [
            f32::MAX,
            f32::MIN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
        ] {
            let result = math::sin(x);
            assert!(
                result.is_finite() && result.abs() <= 1.001,
                "sin({x}) = {result}"
            );
        }
        for x in [-10.0f32, -3.0, 0.0, 3.0, 10.0] {
            assert!((math::sin(x) - x.sin()).abs() < 0.002);
        }
    }

    #[test]
    fn velvet_wraps_out_of_range_tap_delays() {
        let mut engine = Box::new(velvet::VelvetDecorrelator::new());
        engine.init(velvet::Parameters::default());
        engine.current.dynamic = false;
        for delay in [
            1,
            velvet::MAX_DELAY_SAMPLES as u32 + 1,
            2 * velvet::MAX_DELAY_SAMPLES as u32 + 1,
            u32::MAX,
        ] {
            engine.reset();
            let d = delay as usize % velvet::MAX_DELAY_SAMPLES;
            let bank = &mut engine.channels[0].banks[engine.active_bank];
            bank.count = 1;
            bank.storage[0] = velvet::Tap { delay, gain: 0.5 };
            engine.process_sample(0.25, 0.0);
            for _ in 1..d {
                engine.process_sample(0.0, 0.0);
            }
            let (left, _) = engine.process_sample(0.0, 0.0);
            assert_eq!(
                left.to_bits(),
                math::soft_clip(0.125).to_bits(),
                "delay={delay}"
            );

            // The aligned two-history path must apply the same wrap to both
            // the direct and cascaded sends, including malformed tap values.
            engine.reset();
            let bank = &mut engine.channels[0].banks[engine.active_bank];
            bank.count = 1;
            bank.storage[0] = velvet::Tap { delay, gain: 0.5 };
            let mut cascade = velvet::MonoHistory::new();
            engine.process_shared_mono(&mut cascade, 0.25, -0.5);
            for _ in 1..d {
                engine.process_shared_mono(&mut cascade, 0.0, 0.0);
            }
            let ((direct, _), (cascaded, _)) = engine.process_shared_mono(&mut cascade, 0.0, 0.0);
            assert_eq!(
                direct.to_bits(),
                math::soft_clip(0.125).to_bits(),
                "direct delay={delay}"
            );
            assert_eq!(
                cascaded.to_bits(),
                math::soft_clip(-0.25).to_bits(),
                "cascade delay={delay}"
            );
        }
    }

    #[test]
    fn velvet_keeps_the_last_delayed_sample_through_silence() {
        let mut engine = Box::new(velvet::VelvetDecorrelator::new());
        engine.init(velvet::Parameters::default());
        engine.current.dynamic = false;
        for channel in &mut engine.channels {
            let bank = &mut channel.banks[engine.active_bank];
            bank.count = 1;
            bank.storage[0] = velvet::Tap {
                delay: (velvet::MAX_DELAY_SAMPLES - 1) as u32,
                gain: 1.0,
            };
        }
        assert_eq!(engine.process_sample(0.5, -0.5), (0.0, 0.0));
        for _ in 1..velvet::MAX_DELAY_SAMPLES - 1 {
            assert_eq!(engine.process_sample(0.0, 0.0), (0.0, 0.0));
        }
        let (left, right) = engine.process_sample(0.0, 0.0);
        assert_eq!(left.to_bits(), math::soft_clip(0.5).to_bits());
        assert_eq!(right.to_bits(), math::soft_clip(-0.5).to_bits());
        assert_eq!(engine.process_sample(0.0, 0.0), (0.0, 0.0));
    }

    #[test]
    fn velvet_advances_dynamic_filters_during_silence_and_resumes() {
        let mut engine = Box::new(velvet::VelvetDecorrelator::new());
        let mut params = velvet::Parameters::default();
        params.refresh_seconds = 0.1;
        params.transition_ms = 1.0;
        engine.init(params);
        for _ in 0..6_000 {
            assert_eq!(engine.process_sample(0.0, 0.0), (0.0, 0.0));
        }
        assert!(engine.generation > 2);
        let mut peak = 0.0f32;
        for i in 0..velvet::MAX_DELAY_SAMPLES {
            let (left, right) = engine.process_sample(if i == 0 { 0.5 } else { 0.0 }, 0.0);
            peak = peak.max(left.abs()).max(right.abs());
        }
        assert!(peak > 0.0);
    }

    #[test]
    fn velvet_starts_with_the_reference_zero_delay_impulse() {
        let mut engine = Box::new(velvet::VelvetDecorrelator::new());
        let mut params = velvet::Parameters::default();
        params.dynamic = false;
        params.length_ms = 95.0;
        params.density = 100.0;
        engine.init(params);
        // Reference 115dkk/Velvet-Noise-Decorrelator-VST3 at commit
        // a391a12a0b96b190a05cd7cefa25becf4f31660c, 48 kHz, default seed.
        let expected = [[0, 547, 1102, 1391], [0, 426, 1068, 1706]];
        for (channel, delays) in expected.iter().enumerate() {
            let bank = &engine.channels[channel].banks[engine.active_bank];
            assert_eq!(bank.count, 10);
            for (tap, delay) in bank.storage.iter().zip(delays) {
                assert_eq!(tap.delay, *delay, "channel {channel}");
            }
            assert!(bank.storage[0].gain > 0.0);
        }
        let (left, right) = engine.process_sample(0.5, 0.5);
        assert!(left > 0.0 && right > 0.0, "impulse was delayed or inverted");
        params.dynamic = true;
        params.refresh_seconds = 0.1;
        params.transition_ms = 1.0;
        engine.set_parameters(params);
        for _ in 0..15_000 {
            engine.process_sample(0.0, 0.0);
        }
        assert!(engine.generation > 2, "dynamic banks were not regenerated");
        for channel in &engine.channels {
            for bank in &channel.banks {
                assert_eq!(bank.storage[0].delay, 0);
                assert!(bank.storage[0].gain > 0.0);
            }
        }
    }

    #[test]
    fn test_velvet_noise_decorrelator() {
        let mut vnd = velvet::VelvetDecorrelator::new();
        vnd.init(velvet::Parameters::default());

        // Feed an impulse into Left and Right
        let mut out_l = Vec::new();
        let mut out_r = Vec::new();

        // 1 sample impulse, then silence
        let (y_l, y_r) = vnd.process_sample(1.0, 1.0);
        out_l.push(y_l);
        out_r.push(y_r);

        for _ in 1..2000 {
            let (l, r) = vnd.process_sample(0.0, 0.0);
            out_l.push(l);
            out_r.push(r);
        }

        // Calculate energy of wet impulse response (sum of squares)
        let energy_l: f32 = out_l.iter().map(|&x| x * x).sum();
        let energy_r: f32 = out_r.iter().map(|&x| x * x).sum();

        // Unit energy should be close to 1.0 (with dry = 0, amount = 1.0)
        assert!(energy_l > 0.8 && energy_l < 1.2, "Energy L was {energy_l}");
        assert!(energy_r > 0.8 && energy_r < 1.2, "Energy R was {energy_r}");

        // Cross-correlation between Left and Right should be very low (decorrelated)
        let dot_product: f32 = out_l.iter().zip(out_r.iter()).map(|(&l, &r)| l * r).sum();
        let normalized_corr = dot_product / (energy_l * energy_r).sqrt();
        // The reference fixes the first tap of both channels at zero delay;
        // its default normalized zero-lag correlation is about 0.373.
        assert!(
            normalized_corr.abs() < 0.5,
            "Cross-correlation too high: {normalized_corr}"
        );
    }

    #[test]
    fn velvet_density_edits_rebuild_only_when_the_tap_count_changes() {
        let mut edited = Box::new(velvet::VelvetDecorrelator::new());
        let mut reference = Box::new(velvet::VelvetDecorrelator::new());
        let mut params = velvet::Parameters::default();
        params.dynamic = false;
        params.length_ms = 95.0;
        params.density = 2996.0;
        edited.init(params);
        reference.init(params);
        assert_eq!(edited.channels[0].banks[edited.active_bank].count, 285);
        params.density = 3000.0;
        edited.set_parameters(params);
        assert_eq!(edited.current.density, 3000.0);
        assert_eq!(
            edited.generation, reference.generation,
            "unchanged count rebuilt a bank"
        );
        for frame in 0..16_000 {
            let sample = (frame as f32 * 0.031).sin() * 0.2;
            assert_eq!(
                edited.process_sample(sample, sample),
                reference.process_sample(sample, sample),
                "equivalent density edit changed audio at frame {frame}"
            );
        }
        // A spread change still rebuilds the filter.
        params.length_ms = 90.0;
        let before = edited.generation;
        edited.set_parameters(params);
        assert_ne!(edited.generation, before);
        for _ in 0..13_000 {
            edited.process_sample(0.0, 0.0);
        }
        params.density = 100.0;
        let before = edited.generation;
        edited.set_parameters(params);
        assert_ne!(edited.generation, before, "changed count did not rebuild");
        for _ in 0..13_000 {
            edited.process_sample(0.0, 0.0);
        }
        assert_eq!(edited.channels[0].banks[edited.active_bank].count, 9);

        // Rounding can also make two uncapped density requests equivalent.
        params.length_ms = 25.0;
        params.density = 400.0;
        edited.init(params);
        let before = edited.generation;
        params.density = 401.0;
        edited.set_parameters(params);
        assert_eq!(edited.generation, before);
        params.density = 440.0;
        edited.set_parameters(params);
        assert_ne!(edited.generation, before);
        for _ in 0..10 {
            edited.process_sample(0.0, 0.0);
        }
        let building = edited.generation;
        params.density = 441.0;
        edited.set_parameters(params);
        assert_eq!(
            edited.generation, building,
            "equivalent edit restarted a pending build"
        );
    }

    #[test]
    fn test_velvet_dynamic_crossfade() {
        let mut vnd = velvet::VelvetDecorrelator::new();
        let mut params = velvet::Parameters::default();
        params.refresh_seconds = 0.05; // 50 ms fast refresh for test
        params.transition_ms = 10.0; // 10 ms transition
        vnd.init(params);

        // Run audio through multiple renewals and ensure no NaN or blowups
        for i in 0..10000 {
            let sig = ((i as f32) * 0.05).sin();
            let (l, r) = vnd.process_sample(sig, sig);
            assert!(l.is_finite(), "Sample L was not finite at {i}");
            assert!(r.is_finite(), "Sample R was not finite at {i}");
            assert!(l.abs() < 5.0, "Sample L blew up: {l}");
            assert!(r.abs() < 5.0, "Sample R blew up: {r}");
        }
    }

    #[test]
    fn velvet_gain_and_timer_edits_preserve_the_active_filter() {
        let mut reference = velvet::VelvetDecorrelator::new();
        let mut edited = velvet::VelvetDecorrelator::new();
        let mut params = velvet::Parameters::default();
        params.refresh_seconds = 0.1;
        params.transition_ms = 10.0;
        reference.init(params);
        edited.init(params);
        for i in 0..15000 {
            let signal = (i as f32 * 0.1).sin() * 0.1;
            reference.process_sample(signal, signal);
            edited.process_sample(signal, signal);
        }
        params.amount -= 0.000001;
        params.refresh_seconds = 0.2;
        edited.set_parameters(params);
        for i in 15000..16000 {
            let signal = (i as f32 * 0.1).sin() * 0.1;
            let (a_l, a_r) = reference.process_sample(signal, signal);
            let (b_l, b_r) = edited.process_sample(signal, signal);
            assert!((a_l - b_l).abs() < 0.00001);
            assert!((a_r - b_r).abs() < 0.00001);
        }
    }

    #[test]
    fn velvet_filter_edits_crossfade_even_when_dynamic_is_disabled() {
        let mut edited = velvet::VelvetDecorrelator::new();
        let mut reference = velvet::VelvetDecorrelator::new();
        let mut params = velvet::Parameters::default();
        params.dynamic = false;
        params.transition_ms = 10.0;
        edited.init(params);
        reference.init(params);
        for i in 0..2000 {
            let signal = (i as f32 * 0.1).sin() * 0.1;
            edited.process_sample(signal, signal);
            reference.process_sample(signal, signal);
        }
        params.density = 2000.0;
        edited.set_parameters(params);
        let signal = (2000.0f32 * 0.1).sin() * 0.1;
        let first = edited.process_sample(signal, signal);
        let unchanged = reference.process_sample(signal, signal);
        assert!((first.0 - unchanged.0).abs() < 0.00001);
        assert!((first.1 - unchanged.1).abs() < 0.00001);

        // A second edit during the fade must also finish, even in static mode.
        params.density = 500.0;
        edited.set_parameters(params);
        for i in 2001..4000 {
            let signal = (i as f32 * 0.1).sin() * 0.1;
            let (left, right) = edited.process_sample(signal, signal);
            assert!(left.is_finite() && right.is_finite());
        }
        assert!(!edited.transitioning);
        assert!(!edited.filter_update_pending);
        assert_eq!(edited.channels[0].banks[edited.active_bank].count, 14);
    }

    #[test]
    fn velvet_reset_applies_latest_filter_after_edits() {
        for settle in [false, true] {
            let mut edited = velvet::VelvetDecorrelator::new();
            let mut reference = velvet::VelvetDecorrelator::new();
            let mut params = velvet::Parameters::default();
            params.dynamic = false;
            params.transition_ms = 1.0;
            edited.init(params);
            params.density = 500.0;
            edited.set_parameters(params);
            if settle {
                for _ in 0..100 {
                    edited.process_sample(0.0, 0.0);
                }
            }
            edited.reset();
            reference.init(params);
            let mut peak = 0.0f32;
            for i in 0..2000 {
                let input = if i == 0 { 0.5 } else { 0.0 };
                let actual = edited.process_sample(input, input);
                let expected = reference.process_sample(input, input);
                peak = peak.max(expected.0.abs()).max(expected.1.abs());
                assert_eq!(actual, expected, "settled={settle}, frame={i}");
            }
            assert!(peak > 0.001);
        }
    }

    #[test]
    fn velvet_builds_pending_filter_without_exposing_partial_taps() {
        let mut edited = velvet::VelvetDecorrelator::new();
        let mut reference = velvet::VelvetDecorrelator::new();
        let mut params = velvet::Parameters::default();
        params.dynamic = false;
        params.transition_ms = 1.0;
        edited.init(params);
        reference.init(params);
        params.length_ms = 95.0;
        params.density = 3000.0;
        edited.set_parameters(params);

        // A maximum-size bank needs time to prepare. Until it is ready the
        // old filter must produce the same output as an unedited instance.
        for i in 0..100 {
            let before = [
                edited.channels[0].banks[edited.pending_bank],
                edited.channels[1].banks[edited.pending_bank],
            ];
            let input = (i as f32 * 0.13).sin() * 0.1;
            assert_eq!(
                edited.process_sample(input, input),
                reference.process_sample(input, input)
            );
            assert!(!edited.transitioning);
            for c in 0..2 {
                let after = &edited.channels[c].banks[edited.pending_bank];
                let changed = before[c]
                    .storage
                    .iter()
                    .zip(after.storage.iter())
                    .filter(|(a, b)| a.delay != b.delay || a.gain.to_bits() != b.gain.to_bits())
                    .count();
                assert!(
                    changed <= 1,
                    "frame {i}, channel {c}: {changed} taps changed"
                );
            }
        }

        // A newer edit replaces a bank still being built.
        params.density = 200.0;
        edited.set_parameters(params);
        for _ in 0..500 {
            edited.process_sample(0.0, 0.0);
        }
        assert!(!edited.transitioning);
        assert!(!edited.filter_update_pending);
        assert_eq!(edited.channels[0].banks[edited.active_bank].count, 19);
        assert_eq!(edited.channels[1].banks[edited.active_bank].count, 19);
    }

    #[test]
    fn velvet_maximum_density_builds_normalized_banks_and_renews() {
        let mut effect = Box::new(velvet::VelvetDecorrelator::new());
        let params = velvet::Parameters {
            length_ms: 100.0,
            density: 4000.0,
            refresh_seconds: 0.1,
            transition_ms: 1.0,
            ..velvet::Parameters::default()
        };
        effect.init(params);
        let check_banks = |effect: &velvet::VelvetDecorrelator| {
            for channel in &effect.channels {
                for bank in &channel.banks {
                    assert_eq!(bank.count, 400);
                    let taps = &bank.storage[..bank.count];
                    assert_eq!(taps[0].delay, 0);
                    assert!(taps[0].gain > 0.0);
                    assert!(taps.windows(2).all(|pair| pair[0].delay < pair[1].delay));
                    assert!(taps.last().unwrap().delay <= 4800);
                    let energy: f32 = taps.iter().map(|tap| tap.gain * tap.gain).sum();
                    assert!((energy - 1.0).abs() < 0.0001, "energy {energy}");
                }
            }
        };
        check_banks(&effect);
        for i in 0..12_000 {
            let input = (i as f32 * 0.037).sin() * 0.1;
            let (left, right) = effect.process_sample(input, input);
            assert!(left.is_finite() && right.is_finite());
            assert!(left.abs() < 5.0 && right.abs() < 5.0);
        }
        assert!(effect.generation > 2, "maximum-density banks did not renew");
        // Wait until neither bank is being rebuilt before checking normalization.
        effect.set_parameters(velvet::Parameters {
            dynamic: false,
            ..params
        });
        for _ in 0..1000 {
            effect.process_sample(0.0, 0.0);
        }
        check_banks(&effect);
    }

    #[test]
    fn nonfinite_velvet_input_does_not_poison_effect_state() {
        let mut actual = velvet::VelvetDecorrelator::new();
        let mut expected = velvet::VelvetDecorrelator::new();
        actual.init(velvet::Parameters::default());
        expected.init(velvet::Parameters::default());
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
                "velvet frame {frame}"
            );
        }
    }

    #[test]
    fn extreme_finite_velvet_input_does_not_poison_effect_state() {
        let mut actual = velvet::VelvetDecorrelator::new();
        let mut expected = velvet::VelvetDecorrelator::new();
        actual.init(velvet::Parameters::default());
        expected.init(velvet::Parameters::default());
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
                "velvet frame {frame}"
            );
        }
    }
}
