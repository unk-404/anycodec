//! Bitstream helpers shared by the platform backends.
//!
//! Every native codec API wants parameter sets in a different shape:
//!
//! * Android `MediaCodec` takes complete start-code-prefixed NAL units in
//!   `csd-0` / `csd-1`;
//! * Windows Media Foundation takes a single `MF_MT_MPEG_SEQUENCE_HEADER` blob of
//!   start-code-prefixed NALs;
//! * VideoToolbox takes the raw parameter-set pointers plus their lengths.
//!
//! Containers, on the other hand, hand out ISO/IEC 14496-15 configuration
//! records (`avcC` / `hvcC`) with 2-byte big-endian lengths. This module is the
//! one place that understands both, so the three backends do not each grow their
//! own parser.

use crate::media::codec::{Codec, VideoCodec};

/// Converts a decoder configuration record into start-code-prefixed NAL units.
///
/// Accepts either an `avcC` / `hvcC` record or an already Annex-B buffer. In both
/// cases the returned units **carry a four-byte start code** and a caller that
/// wants the bare NAL payload (VideoToolbox wants the pointer and the length
/// separately) must strip the first four bytes itself.
///
/// Returns an empty vector for an empty or unrecognised input. The caller then
/// configures the platform decoder without out-of-band parameter sets, which
/// works whenever the stream carries them in band.
#[must_use]
pub fn annex_b_parameter_sets(codec: Codec, extradata: &[u8], already_annex_b: bool) -> Vec<Vec<u8>> {
    if extradata.is_empty() {
        return Vec::new();
    }
    if already_annex_b || has_start_code(extradata) {
        return split_annex_b(extradata);
    }
    match codec {
        Codec::Video(VideoCodec::H264) => parse_avcc(extradata),
        Codec::Video(VideoCodec::H265) => parse_hvcc(extradata),
        _ => Vec::new(),
    }
}

/// All parameter sets concatenated into one Annex-B blob.
///
/// This is the shape Media Foundation and the Apple frameworks want.
#[must_use]
pub fn annex_b_blob(codec: Codec, extradata: &[u8], already_annex_b: bool) -> Vec<u8> {
    let sets = annex_b_parameter_sets(codec, extradata, already_annex_b);
    let mut blob = Vec::with_capacity(sets.iter().map(Vec::len).sum());
    for set in sets {
        blob.extend_from_slice(&set);
    }
    blob
}

/// `true` when the buffer starts with an Annex-B start code.
#[must_use]
pub fn has_start_code(bytes: &[u8]) -> bool {
    bytes.windows(3).take(16).any(|window| window == [0, 0, 1])
}

/// Splits an Annex-B buffer into units that still carry their start code.
#[must_use]
pub fn split_annex_b(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut units = Vec::new();
    let mut index = 0usize;
    let mut start = None;
    while index + 2 < bytes.len() {
        let is_three = bytes[index] == 0 && bytes[index + 1] == 0 && bytes[index + 2] == 1;
        let is_four = index + 3 < bytes.len()
            && bytes[index] == 0
            && bytes[index + 1] == 0
            && bytes[index + 2] == 0
            && bytes[index + 3] == 1;
        if is_three || is_four {
            if let Some(previous) = start {
                if index > previous {
                    units.push(bytes[previous..index].to_vec());
                }
            }
            index += if is_four { 4 } else { 3 };
            start = Some(index);
        } else {
            index += 1;
        }
    }
    if let Some(previous) = start {
        if previous < bytes.len() {
            units.push(bytes[previous..].to_vec());
        }
    }
    units
}

/// Parses an ISO/IEC 14496-15 `AVCDecoderConfigurationRecord`.
#[must_use]
pub fn parse_avcc(record: &[u8]) -> Vec<Vec<u8>> {
    if record.len() < 7 || record[0] != 1 {
        return Vec::new();
    }
    let mut units = Vec::new();
    let mut cursor = 5usize;
    let sps_count = (record[cursor] & 0x1f) as usize;
    cursor += 1;
    read_length_prefixed(record, &mut cursor, sps_count, &mut units);
    if cursor >= record.len() {
        return units;
    }
    let pps_count = record[cursor] as usize;
    cursor += 1;
    read_length_prefixed(record, &mut cursor, pps_count, &mut units);
    units
}

/// Parses an ISO/IEC 14496-15 `HEVCDecoderConfigurationRecord`.
#[must_use]
pub fn parse_hvcc(record: &[u8]) -> Vec<Vec<u8>> {
    if record.len() < 23 || record[0] != 1 {
        return Vec::new();
    }
    let mut units = Vec::new();
    let array_count = record[22] as usize;
    let mut cursor = 23usize;
    for _ in 0..array_count {
        if cursor + 3 > record.len() {
            return units;
        }
        // record[cursor] carries completeness/reserved/NAL_unit_type.
        let nal_count = ((record[cursor + 1] as usize) << 8) | record[cursor + 2] as usize;
        cursor += 3;
        read_length_prefixed(record, &mut cursor, nal_count, &mut units);
    }
    units
}

fn read_length_prefixed(
    record: &[u8],
    cursor: &mut usize,
    count: usize,
    units: &mut Vec<Vec<u8>>,
) {
    for _ in 0..count {
        if *cursor + 2 > record.len() {
            return;
        }
        let length = ((record[*cursor] as usize) << 8) | record[*cursor + 1] as usize;
        *cursor += 2;
        if *cursor + length > record.len() {
            return;
        }
        units.push(prefix_with_start_code(&record[*cursor..*cursor + length]));
        *cursor += length;
    }
}

fn prefix_with_start_code(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len() + 4);
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(nal);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const AVCC: [u8; 16] = [
        0x01, 0x64, 0x00, 0x1f, 0xff, 0xe1, 0x00, 0x03, 0x67, 0x64, 0x00, 0x01, 0x00, 0x02, 0x68,
        0xee,
    ];

    #[test]
    fn avcc_becomes_annex_b() {
        let sets = annex_b_parameter_sets(Codec::Video(VideoCodec::H264), &AVCC, false);
        assert_eq!(sets.len(), 2);
        assert_eq!(sets[0], vec![0, 0, 0, 1, 0x67, 0x64, 0x00]);
        assert_eq!(sets[1], vec![0, 0, 0, 1, 0x68, 0xee]);
    }

    #[test]
    fn the_blob_concatenates_every_unit() {
        let blob = annex_b_blob(Codec::Video(VideoCodec::H264), &AVCC, false);
        assert_eq!(blob.len(), 7 + 6);
        assert!(has_start_code(&blob));
    }

    #[test]
    fn annex_b_input_is_split_not_reinterpreted() {
        let stream = [0u8, 0, 0, 1, 0x67, 0x11, 0, 0, 1, 0x68, 0x22];
        let sets = annex_b_parameter_sets(Codec::Video(VideoCodec::H264), &stream, true);
        assert_eq!(sets, vec![vec![0x67, 0x11], vec![0x68, 0x22]]);
    }

    #[test]
    fn a_start_code_is_detected_without_the_hint() {
        let stream = [0u8, 0, 1, 0x67, 0x11];
        assert!(has_start_code(&stream));
        assert_eq!(
            annex_b_parameter_sets(Codec::Video(VideoCodec::H265), &stream, false),
            vec![vec![0x67, 0x11]]
        );
    }

    #[test]
    fn empty_and_unrecognised_input_yields_nothing() {
        assert!(annex_b_parameter_sets(Codec::Video(VideoCodec::H264), &[], false).is_empty());
        // An avcC claiming no parameter sets.
        assert!(
            annex_b_parameter_sets(
                Codec::Video(VideoCodec::H264),
                &[0x01, 0x64, 0x00, 0x1f, 0xff, 0xe0, 0x00],
                false
            )
            .is_empty()
        );
        // AAC extradata is not a NAL record.
        assert!(
            annex_b_parameter_sets(Codec::Audio(crate::media::codec::AudioCodec::Aac), &[0x12, 0x10], false)
                .is_empty()
        );
    }

    #[test]
    fn hvcc_becomes_annex_b() {
        let mut hvcc = vec![0u8; 23];
        hvcc[0] = 1;
        hvcc[22] = 1;
        hvcc.push(0x80 | 32);
        hvcc.extend_from_slice(&[0, 1]);
        hvcc.extend_from_slice(&[0, 2, 0x40, 0x01]);
        assert_eq!(
            annex_b_parameter_sets(Codec::Video(VideoCodec::H265), &hvcc, false),
            vec![vec![0, 0, 0, 1, 0x40, 0x01]]
        );
    }

    #[test]
    fn a_truncated_record_stops_instead_of_panicking() {
        for length in 0..AVCC.len() {
            let _ = parse_avcc(&AVCC[..length]);
        }
        let mut hvcc = vec![0u8; 23];
        hvcc[0] = 1;
        hvcc[22] = 8;
        let _ = parse_hvcc(&hvcc);
    }
}
