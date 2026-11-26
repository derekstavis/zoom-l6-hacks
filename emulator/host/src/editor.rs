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

/// Decode the first file-count reply for the requested pad.
pub fn pad_file_count(replies: &[Vec<u8>], pad: u8) -> Option<u16> {
    replies.iter().find_map(|reply| {
        (reply.get(4..7) == Some(&[0x45, 0x00, pad]) && reply.len() >= 10)
            .then(|| ((reply[7] as u16) << 7) | reply[8] as u16)
    })
}

/// Validate the fields consumed from a filename reply.
pub fn pad_filename_valid(reply: &[u8], pad: u8) -> bool {
    reply.get(4..7) == Some(&[0x45, 0x01, pad])
        && reply.len() >= 12
        && reply.get(7..9) != Some(&[0x7f, 0x7f])
        && reply.get(9..11) != Some(&[0, 0])
}

/// Match the state reply's exact assignment bytes against the chosen file.
pub fn pad_assignment_matches(replies: &[Vec<u8>], pad: u8, args: &[u8]) -> bool {
    replies.iter().any(|reply| {
        reply.get(4..7) == Some(&[0x45, 0x02, pad])
            && reply.get(6..reply.len().saturating_sub(1)) == Some(args)
    })
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
