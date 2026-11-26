//! Typed observations of Editor protocol messages.

/// Check the pad, file index and entry bytes in an Editor assignment reply.
/// The firmware uses 0x7f7f as the unassigned index and zero as an empty entry.
pub fn pad_assignment_present(reply: &[u8], pad: u8) -> bool {
    pad < 4
        && reply.get(4..7) == Some(&[0x45, 0x02, pad])
        && reply.len() >= 12
        && reply.get(7..9) != Some(&[0x7f, 0x7f])
        && reply.get(9..11) != Some(&[0, 0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_pad_length_unassigned_index_and_empty_entry() {
        let good = [0xf0, 0x52, 0, 0, 0x45, 0x02, 2, 1, 0, 1, 0, 0xf7];
        assert!(pad_assignment_present(&good, 2));
        assert!(!pad_assignment_present(&good, 1));
        assert!(!pad_assignment_present(&good[..11], 2));
        let mut missing = good;
        missing[7..9].copy_from_slice(&[0x7f, 0x7f]);
        assert!(!pad_assignment_present(&missing, 2));
        missing = good;
        missing[9..11].copy_from_slice(&[0, 0]);
        assert!(!pad_assignment_present(&missing, 2));
    }
}
