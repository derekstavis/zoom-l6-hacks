//! RIFF/WAVE container traversal shared by audio input and analysis.

/// Iterate over chunk payloads in file order. Truncated payloads are clamped to
/// the available bytes, matching the existing audio readers. Odd-sized chunks
/// consume a padding byte when present.
pub struct Chunks<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Chunks<'a> {
    /// Return `None` for data without a RIFF/WAVE header.
    pub fn new(bytes: &'a [u8]) -> Option<Self> {
        (bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE")
            .then_some(Self { bytes, offset: 12 })
    }
}

impl<'a> Iterator for Chunks<'a> {
    type Item = (&'a [u8], &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let header_end = self.offset.checked_add(8)?;
        if header_end > self.bytes.len() {
            return None;
        }
        let id = &self.bytes[self.offset..self.offset + 4];
        let size =
            u32::from_le_bytes(self.bytes[self.offset + 4..header_end].try_into().ok()?) as usize;
        let end = header_end.saturating_add(size).min(self.bytes.len());
        self.offset = header_end.saturating_add(size).saturating_add(size & 1);
        Some((id, &self.bytes[header_end..end]))
    }
}

#[cfg(test)]
mod tests {
    use super::Chunks;

    #[test]
    fn walks_padded_and_truncated_chunks() {
        let mut bytes = b"RIFF\0\0\0\0WAVEJUNK\x03\0\0\0abc\0data\x06\0\0\0xy".to_vec();
        let chunks = Chunks::new(&bytes).unwrap().collect::<Vec<_>>();
        assert_eq!(
            chunks,
            vec![(&b"JUNK"[..], &b"abc"[..]), (&b"data"[..], &b"xy"[..])]
        );
        bytes.truncate(10);
        assert!(Chunks::new(&bytes).is_none());
    }
}
