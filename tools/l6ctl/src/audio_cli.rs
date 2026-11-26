//! Offline audio measurements; no emulator or control socket is required.
use l6ctl::audio_analysis;
use serde_json::{Value, json};
use std::fs;

const USAGE: &str =
    "usage: l6ctl audio compare SOURCE.wav CAPTURE.s32le | l6ctl audio tone CAPTURE.s32le HZ RATE";

fn capture(path: &str) -> Result<Vec<[f64; 2]>, String> {
    let bytes = fs::read(path).map_err(|error| format!("cannot read capture {path}: {error}"))?;
    if bytes.is_empty() || bytes.len() % 8 != 0 {
        return Err("capture must contain complete stereo S32LE frames".into());
    }
    Ok(audio_analysis::decode_capture(&bytes))
}

pub fn run(args: &[String]) -> Result<Value, String> {
    match args {
        [command, source, captured] if command == "compare" => {
            let bytes = fs::read(source)
                .map_err(|error| format!("cannot read source {source}: {error}"))?;
            let frames = audio_analysis::decode_wav(&bytes)?;
            let comparison = audio_analysis::compare(&frames, &capture(captured)?)?;
            serde_json::to_value(comparison).map_err(|error| error.to_string())
        }
        [command, captured, hz, rate] if command == "tone" => {
            let hz: f64 = hz.parse().map_err(|_| "invalid frequency")?;
            let rate: f64 = rate.parse().map_err(|_| "invalid sample rate")?;
            if !hz.is_finite() || !rate.is_finite() || hz <= 0.0 || rate <= 0.0 || hz > rate / 2.0 {
                return Err("frequency and rate must be finite and positive; frequency must not exceed Nyquist".into());
            }
            let frames = capture(captured)?;
            Ok(
                json!({"frequency_hz":hz,"sample_rate_hz":rate,"frames":frames.len(),"levels_dbfs":audio_analysis::tone_levels_db(&frames, hz, rate)}),
            )
        }
        _ => Err(USAGE.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compares_files_and_reports_tone_levels() {
        let path = std::env::temp_dir().join(format!("l6ctl-audio-test-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        let source = path.join("source.wav");
        let raw = path.join("capture.s32le");
        let samples: Vec<i16> = (0..480)
            .map(|i| ((std::f64::consts::TAU * i as f64 / 48.0).sin() * 12000.0) as i16)
            .collect();
        let mut wav = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0".to_vec();
        wav.extend(1u16.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(48000u32.to_le_bytes());
        wav.extend(96000u32.to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(((samples.len() * 2) as u32).to_le_bytes());
        let mut captured = Vec::new();
        for sample in samples {
            wav.extend(sample.to_le_bytes());
            let value = (i32::from(sample) << 16).to_le_bytes();
            captured.extend(value);
            captured.extend(value);
        }
        fs::write(&source, wav).unwrap();
        fs::write(&raw, captured).unwrap();
        let source = source.to_str().unwrap().to_owned();
        let raw = raw.to_str().unwrap().to_owned();
        let result = run(&["compare".into(), source, raw.clone()]).unwrap();
        assert_eq!(result["offset_frames"], 0);
        assert!(result["residual_db"].as_f64().unwrap() < -100.0);
        let tone = run(&["tone".into(), raw, "1000".into(), "48000".into()]).unwrap();
        assert_eq!(tone["frames"], 480);
        assert!(tone["levels_dbfs"][0].as_f64().unwrap() > -10.0);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn validates_arguments_before_opening_capture() {
        assert!(run(&[]).unwrap_err().contains("usage:"));
        assert!(
            run(&[
                "tone".into(),
                "missing".into(),
                "NaN".into(),
                "48000".into()
            ])
            .unwrap_err()
            .contains("finite")
        );
        assert!(
            run(&[
                "tone".into(),
                "missing".into(),
                "25000".into(),
                "48000".into()
            ])
            .unwrap_err()
            .contains("Nyquist")
        );
    }
}
