//! Decode QEMU's bounded sound-pad SAI sample observations.
use serde::{Deserialize, Serialize};

pub const SAI1_TX_PREFIX: &str = "l6 audio TX samples sai=00384000";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxSample {
    pub words: u64,
    pub nonzero: u64,
    pub underruns: u64,
    pub first: String,
    pub last: String,
    pub at_ms: u64,
}

/// Preserve trace line ordering and skip malformed observations, as the sound-pad probe does.
pub fn sai1_tx_samples(trace: &str, skip: usize) -> Vec<TxSample> {
    trace
        .lines()
        .filter(|line| line.starts_with(SAI1_TX_PREFIX))
        .skip(skip)
        .filter_map(|line| {
            let field = |name: &str| {
                line.split_whitespace()
                    .find_map(|part| part.strip_prefix(&format!("{name}=")))
            };
            Some(TxSample {
                words: field("words")?.parse().ok()?,
                nonzero: field("nonzero")?.parse().ok()?,
                underruns: field("underruns")?.parse().ok()?,
                first: field("first")?.to_owned(),
                last: field("last")?.to_owned(),
                at_ms: field("at")?.strip_suffix("ms")?.parse().ok()?,
            })
        })
        .collect()
}

pub fn sai1_tx_nonzero(samples: &[TxSample]) -> bool {
    samples.iter().any(|sample| {
        sample.nonzero > 0
            && sample.words > 0
            && sample.first != "ffffffff"
            && sample.last != "ffffffff"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn skips_baseline_and_malformed_records_and_rejects_sentinel_samples() {
        let trace = "l6 audio TX samples sai=00384000 words=9 nonzero=3 underruns=0 first=00000001 last=00000002 at=4ms\nl6 audio TX samples sai=00384000 words=9 nonzero=3 underruns=0 first=ffffffff last=00000002 at=5ms\nl6 audio TX samples sai=00384000 words=broken\n";
        let samples = sai1_tx_samples(trace, 1);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].at_ms, 5);
        assert!(!sai1_tx_nonzero(&samples));
        assert!(sai1_tx_nonzero(&sai1_tx_samples(trace, 0)));
    }
}
