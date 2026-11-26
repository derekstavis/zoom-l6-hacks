//! Typed observations of Editor protocol messages.

/// Requests used by the Editor's pad-assignment and playback protocol.
/// Pad and file fields are the protocol's zero-based values; the caller owns
/// model-specific validation and sequencing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Identity,
    GlobalSettings,
    ApplicationVersion,
    SdInfo,
    KeepAlive,
    PadFileCount { pad: u8 },
    PadFilename { pad: u8 },
    PadAssignment { pad: u8 },
    PadPlaybackState { pad: u8 },
    PlayPad { pad: u8 },
    AssignPad { args: Vec<u8> },
    AcceptPadDialog,
}

impl Request {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Identity => vec![0xf0, 0x7e, 0, 0x06, 0x01, 0xf7],
            Self::GlobalSettings => vec![0xf0, 0x52, 0, 0, 0x2b, 0xf7],
            Self::ApplicationVersion => vec![
                0xf0, 0x52, 0, 0, 0x45, 0x05, 0x02, 0, 0, 0, 0, 0, 0x27, 0, 0xf7,
            ],
            Self::SdInfo => vec![0xf0, 0x52, 0, 0, 0x67, 0x01, 0xf7],
            Self::KeepAlive => vec![0xf0, 0x52, 0, 0, 0x31, 0x0b, 0xf7],
            Self::PadFileCount { pad } => vec![0xf0, 0x52, 0, 0, 0x46, 0, *pad, 0xf7],
            Self::PadFilename { pad } => vec![0xf0, 0x52, 0, 0, 0x46, 1, *pad, 0, 0, 0xf7],
            Self::PadAssignment { pad } => vec![0xf0, 0x52, 0, 0, 0x46, 2, *pad, 0xf7],
            Self::PadPlaybackState { pad } => vec![0xf0, 0x52, 0, 0, 0x46, 3, *pad, 0xf7],
            Self::PlayPad { pad } => vec![0xf0, 0x52, 0, 0, 0x31, 0x08, *pad, 1, 0xf7],
            Self::AssignPad { args } => {
                let mut message = vec![0xf0, 0x52, 0, 0, 0x31, 0x05];
                message.extend_from_slice(args);
                message.push(0xf7);
                message
            }
            Self::AcceptPadDialog => vec![0xf0, 0x52, 0, 0, 0x31, 0x12, 0x06, 0x01, 0xf7],
        }
    }
}

/// The firmware has two reply forms for the pad confirmation dialog.
pub fn pad_dialog_pending(replies: &[Vec<u8>]) -> bool {
    replies.iter().any(|reply| {
        reply.get(4..6) == Some(&[0x45, 0x06]) || reply.get(4..7) == Some(&[0x31, 0x10, 0x06])
    })
}

/// Whether a reply reports that this pad is playing.
pub fn pad_playing(replies: &[Vec<u8>], pad: u8) -> bool {
    replies
        .iter()
        .any(|reply| reply.get(4..8) == Some(&[0x45, 0x03, pad, 0x01]))
}

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
    fn request_encodings_match_editor_wire_protocol() {
        let requests = [
            (Request::Identity, vec![0xf0, 0x7e, 0, 6, 1, 0xf7]),
            (Request::GlobalSettings, vec![0xf0, 0x52, 0, 0, 0x2b, 0xf7]),
            (
                Request::ApplicationVersion,
                vec![0xf0, 0x52, 0, 0, 0x45, 5, 2, 0, 0, 0, 0, 0, 0x27, 0, 0xf7],
            ),
            (Request::SdInfo, vec![0xf0, 0x52, 0, 0, 0x67, 1, 0xf7]),
            (Request::KeepAlive, vec![0xf0, 0x52, 0, 0, 0x31, 0x0b, 0xf7]),
            (
                Request::PadFileCount { pad: 2 },
                vec![0xf0, 0x52, 0, 0, 0x46, 0, 2, 0xf7],
            ),
            (
                Request::PadFilename { pad: 2 },
                vec![0xf0, 0x52, 0, 0, 0x46, 1, 2, 0, 0, 0xf7],
            ),
            (
                Request::PadAssignment { pad: 2 },
                vec![0xf0, 0x52, 0, 0, 0x46, 2, 2, 0xf7],
            ),
            (
                Request::PadPlaybackState { pad: 2 },
                vec![0xf0, 0x52, 0, 0, 0x46, 3, 2, 0xf7],
            ),
            (
                Request::PlayPad { pad: 2 },
                vec![0xf0, 0x52, 0, 0, 0x31, 8, 2, 1, 0xf7],
            ),
            (
                Request::AssignPad {
                    args: vec![2, 3, 4],
                },
                vec![0xf0, 0x52, 0, 0, 0x31, 5, 2, 3, 4, 0xf7],
            ),
            (
                Request::AcceptPadDialog,
                vec![0xf0, 0x52, 0, 0, 0x31, 0x12, 6, 1, 0xf7],
            ),
        ];
        for (request, expected) in requests {
            assert_eq!(request.encode(), expected, "{request:?}");
        }
    }

    #[test]
    fn recognizes_dialog_forms_and_pad_playback_without_accepting_partial_replies() {
        assert!(pad_dialog_pending(&[vec![0xf0, 0x52, 0, 0, 0x45, 6]]));
        assert!(pad_dialog_pending(&[vec![0xf0, 0x52, 0, 0, 0x31, 0x10, 6]]));
        assert!(!pad_dialog_pending(&[vec![0xf0, 0x52, 0, 0, 0x31, 0x10]]));
        let reply = vec![0xf0, 0x52, 0, 0, 0x45, 3, 2, 1];
        assert!(pad_playing(std::slice::from_ref(&reply), 2));
        assert!(!pad_playing(std::slice::from_ref(&reply), 1));
        assert!(!pad_playing(&[reply[..7].to_vec()], 2));
    }

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
