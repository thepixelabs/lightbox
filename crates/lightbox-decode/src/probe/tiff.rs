// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Bounded TIFF/IFD walker — the metadata heart of the raw probe (T19).
//!
//! One walker covers every TIFF-shaped container E01 meets: CR2, NEF, ARW,
//! ORF (Olympus' `RO`/`RS` magic variants), DNG, plain TIFF, and the
//! little TIFF blobs embedded elsewhere (CR3 `CMT1`/`CMT2` boxes). It reads
//! **structures only** — IFD tables and the handful of tag values probing
//! needs — never image payloads, which is what keeps `probe()` metadata-only
//! (spec §3.7: no full read).
//!
//! Safety posture (fuzz-seeded, spec T19): every read is bounds-checked and
//! size-capped, IFD count/entry count/queue depth are hard-limited, and
//! offset cycles are broken by a visited set. Malformed structure is
//! [`ProbeError::Malformed`], **never** a panic.
//!
//! Why not rawler (the spec's suggestion): rawler is LGPL-2.1, which the
//! crate-graph license gate denies by construction (`deny.toml`,
//! architecture §1.6). See `E01-deviations.md` (Phase 5).

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};

use crate::ProbeError;

/// Standard TIFF magic (`42`).
pub(crate) const MAGIC_TIFF: u16 = 42;
/// Olympus ORF magic variant `"RO"` (e.g. E-1).
pub(crate) const MAGIC_ORF_RO: u16 = 0x4F52;
/// Olympus ORF magic variant `"RS"` (newer bodies).
pub(crate) const MAGIC_ORF_RS: u16 = 0x5352;

/// Every IFD visited is capped; raws carry < 10 relevant IFDs.
const MAX_IFDS: usize = 32;
/// Entries per IFD (cameras write dozens; MakerNotes are never walked).
const MAX_ENTRIES: usize = 512;
/// Longest ASCII value read (Make/Model/DateTime are all far shorter).
const MAX_ASCII: usize = 96;
/// Most numeric values read per tag (`SubIFDs` and 1-strip offsets suffice).
const MAX_UINTS: usize = 8;

/// Which chain an IFD was discovered on.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum IfdKind {
    /// IFD0 and its `next` chain.
    Primary,
    /// A `SubIFDs` (0x014A) child.
    Sub,
    /// An `Exif IFD` (0x8769) child.
    Exif,
}

/// The probe-relevant slice of one IFD.
#[derive(Clone, Debug)]
pub(crate) struct IfdData {
    /// Discovery chain.
    pub kind: IfdKind,
    /// `ImageWidth` (0x0100).
    pub width: Option<u32>,
    /// `ImageLength` (0x0101).
    pub height: Option<u32>,
    /// `Compression` (0x0103).
    pub compression: Option<u16>,
    /// `PhotometricInterpretation` (0x0106).
    pub photometric: Option<u16>,
    /// `Make` (0x010F).
    pub make: Option<String>,
    /// `Model` (0x0110).
    pub model: Option<String>,
    /// `Orientation` (0x0112), raw EXIF value.
    pub orientation: Option<u16>,
    /// `DateTime` (0x0132) — modification time; capture-time fallback.
    pub date_time: Option<String>,
    /// `DateTimeOriginal` (0x9003).
    pub date_time_original: Option<String>,
    /// `DateTimeDigitized` (0x9004).
    pub date_time_digitized: Option<String>,
    /// `OffsetTimeOriginal` (0x9011), e.g. `"+01:00"`.
    pub offset_time_original: Option<String>,
    /// `PixelXDimension` (0xA002) — the authoritative output width.
    pub pixel_x: Option<u32>,
    /// `PixelYDimension` (0xA003).
    pub pixel_y: Option<u32>,
    /// `DNGVersion` (0xC612) present?
    pub dng_version: bool,
    /// Exactly-one-strip payload: `(StripOffsets[0], StripByteCounts[0])`
    /// when both tags have count == 1.
    pub single_strip: Option<(u64, u64)>,
    /// `(JPEGInterchangeFormat, JPEGInterchangeFormatLength)` (0x0201/0x0202).
    pub jpeg_range: Option<(u64, u64)>,
}

impl IfdData {
    fn new(kind: IfdKind) -> IfdData {
        IfdData {
            kind,
            width: None,
            height: None,
            compression: None,
            photometric: None,
            make: None,
            model: None,
            orientation: None,
            date_time: None,
            date_time_original: None,
            date_time_digitized: None,
            offset_time_original: None,
            pixel_x: None,
            pixel_y: None,
            dng_version: false,
            single_strip: None,
            jpeg_range: None,
        }
    }
}

/// A fully walked TIFF structure.
#[derive(Debug)]
pub(crate) struct TiffWalk {
    /// Header magic (42, or an ORF variant).
    pub magic: u16,
    /// All visited IFDs in discovery order (primary chain first).
    pub ifds: Vec<IfdData>,
}

impl TiffWalk {
    /// First `Some` of `f` over the IFDs, `Exif` IFDs preferred (the CR3
    /// `CMT2` blob stores Exif tags in its primary IFD, so primaries are
    /// consulted next), then subs.
    pub(crate) fn first<T>(&self, f: impl Fn(&IfdData) -> Option<T>) -> Option<T> {
        let by_kind = |kind: IfdKind| self.ifds.iter().filter(move |i| i.kind == kind);
        by_kind(IfdKind::Exif)
            .chain(by_kind(IfdKind::Primary))
            .chain(by_kind(IfdKind::Sub))
            .find_map(f)
    }
}

fn malformed(msg: impl Into<String>) -> ProbeError {
    ProbeError::Malformed(msg.into())
}

/// Seek + exact read; a short read is [`ProbeError::Malformed`] (truncated
/// structure), any other IO failure stays [`ProbeError::Io`].
fn read_at<R: Read + Seek>(r: &mut R, off: u64, buf: &mut [u8]) -> Result<(), ProbeError> {
    r.seek(SeekFrom::Start(off)).map_err(ProbeError::Io)?;
    r.read_exact(buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            malformed(format!("truncated structure at byte {off}"))
        } else {
            ProbeError::Io(e)
        }
    })
}

fn u16_from(bytes: &[u8], le: bool) -> u16 {
    let b: [u8; 2] = [bytes[0], bytes[1]];
    if le {
        u16::from_le_bytes(b)
    } else {
        u16::from_be_bytes(b)
    }
}

fn u32_from(bytes: &[u8], le: bool) -> u32 {
    let b: [u8; 4] = [bytes[0], bytes[1], bytes[2], bytes[3]];
    if le {
        u32::from_le_bytes(b)
    } else {
        u32::from_be_bytes(b)
    }
}

/// Bytes per element of a TIFF field type; `None` for unknown types
/// (entry skipped, not an error — future types must not break probing).
fn type_size(typ: u16) -> Option<u64> {
    Some(match typ {
        1 | 2 | 6 | 7 => 1,         // BYTE, ASCII, SBYTE, UNDEFINED
        3 | 8 => 2,                 // SHORT, SSHORT
        4 | 9 | 11 | 13 => 4,       // LONG, SLONG, FLOAT, IFD
        5 | 10 | 12 | 16..=18 => 8, // RATIONAL, SRATIONAL, DOUBLE, LONG8…
        _ => return None,
    })
}

/// One raw IFD entry: tag, type, count, and the 4 inline value/offset bytes.
struct RawEntry {
    tag: u16,
    typ: u16,
    count: u32,
    inline: [u8; 4],
}

impl RawEntry {
    /// The value bytes: inline when they fit in 4 bytes, otherwise read from
    /// the pointed-to offset. Capped at `max_len`.
    fn value_bytes<R: Read + Seek>(
        &self,
        r: &mut R,
        le: bool,
        max_len: usize,
    ) -> Result<Vec<u8>, ProbeError> {
        let elem = match type_size(self.typ) {
            Some(s) => s,
            None => return Ok(Vec::new()),
        };
        let total = elem.saturating_mul(u64::from(self.count));
        let take = usize::try_from(total.min(max_len as u64)).unwrap_or(max_len);
        if total <= 4 {
            return Ok(self.inline[..take.min(4)].to_vec());
        }
        let off = u64::from(u32_from(&self.inline, le));
        let mut buf = vec![0u8; take];
        read_at(r, off, &mut buf)?;
        Ok(buf)
    }

    /// Unsigned integers (SHORT/LONG only), up to [`MAX_UINTS`] values.
    fn uints<R: Read + Seek>(&self, r: &mut R, le: bool) -> Result<Vec<u32>, ProbeError> {
        let elem = match self.typ {
            3 => 2usize,
            4 | 13 => 4usize,
            _ => return Ok(Vec::new()),
        };
        let bytes = self.value_bytes(r, le, elem * MAX_UINTS)?;
        Ok(bytes
            .chunks_exact(elem)
            .map(|c| {
                if elem == 2 {
                    u32::from(u16_from(c, le))
                } else {
                    u32_from(c, le)
                }
            })
            .collect())
    }

    fn uint<R: Read + Seek>(&self, r: &mut R, le: bool) -> Result<Option<u32>, ProbeError> {
        Ok(self.uints(r, le)?.first().copied())
    }

    /// ASCII value, NUL-terminated, whitespace-trimmed; `None` when empty.
    fn ascii<R: Read + Seek>(&self, r: &mut R, le: bool) -> Result<Option<String>, ProbeError> {
        if self.typ != 2 {
            return Ok(None);
        }
        let bytes = self.value_bytes(r, le, MAX_ASCII)?;
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        let s = String::from_utf8_lossy(&bytes[..end]).trim().to_owned();
        Ok((!s.is_empty()).then_some(s))
    }
}

/// Walks a TIFF structure from byte 0 of `r` (rewind or wrap a `Cursor`
/// first for embedded blobs). Collects the primary IFD chain, `SubIFDs`,
/// and `Exif IFD`s. MakerNotes are deliberately never entered.
pub(crate) fn walk<R: Read + Seek>(r: &mut R) -> Result<TiffWalk, ProbeError> {
    let mut header = [0u8; 8];
    read_at(r, 0, &mut header)?;
    let le = match &header[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return Err(malformed("no TIFF byte-order marker")),
    };
    let magic = u16_from(&header[2..4], le);
    if !matches!(magic, MAGIC_TIFF | MAGIC_ORF_RO | MAGIC_ORF_RS) {
        return Err(malformed(format!(
            "unknown TIFF-family magic 0x{magic:04x}"
        )));
    }
    let ifd0 = u64::from(u32_from(&header[4..8], le));

    let mut ifds = Vec::new();
    let mut visited: HashSet<u64> = HashSet::new();
    // (offset, kind, follow_next_chain)
    let mut queue: Vec<(u64, IfdKind, bool)> = vec![(ifd0, IfdKind::Primary, true)];

    while let Some((off, kind, follow_chain)) = queue.pop() {
        if off == 0 || !visited.insert(off) || ifds.len() >= MAX_IFDS {
            continue;
        }
        let mut count_buf = [0u8; 2];
        read_at(r, off, &mut count_buf)?;
        let n = usize::from(u16_from(&count_buf, le));
        if n > MAX_ENTRIES {
            return Err(malformed(format!("IFD at {off} claims {n} entries")));
        }
        let mut table = vec![0u8; 12 * n + 4];
        read_at(r, off + 2, &mut table)?;

        let mut ifd = IfdData::new(kind);
        let mut strip_offsets: Option<Vec<u32>> = None;
        let mut strip_counts: Option<Vec<u32>> = None;
        let mut jpeg_off: Option<u32> = None;
        let mut jpeg_len: Option<u32> = None;

        for i in 0..n {
            let e = &table[12 * i..12 * i + 12];
            let entry = RawEntry {
                tag: u16_from(&e[0..2], le),
                typ: u16_from(&e[2..4], le),
                count: u32_from(&e[4..8], le),
                inline: [e[8], e[9], e[10], e[11]],
            };
            match entry.tag {
                0x0100 => ifd.width = entry.uint(r, le)?,
                0x0101 => ifd.height = entry.uint(r, le)?,
                0x0103 => ifd.compression = entry.uint(r, le)?.map(|v| v as u16),
                0x0106 => ifd.photometric = entry.uint(r, le)?.map(|v| v as u16),
                0x010F => ifd.make = entry.ascii(r, le)?,
                0x0110 => ifd.model = entry.ascii(r, le)?,
                0x0111 if entry.count == 1 => strip_offsets = Some(entry.uints(r, le)?),
                0x0112 => ifd.orientation = entry.uint(r, le)?.map(|v| v as u16),
                0x0117 if entry.count == 1 => strip_counts = Some(entry.uints(r, le)?),
                0x0132 => ifd.date_time = entry.ascii(r, le)?,
                0x014A => {
                    for sub in entry.uints(r, le)? {
                        queue.push((u64::from(sub), IfdKind::Sub, false));
                    }
                }
                0x0201 => jpeg_off = entry.uint(r, le)?,
                0x0202 => jpeg_len = entry.uint(r, le)?,
                0x8769 => {
                    if let Some(exif) = entry.uint(r, le)? {
                        queue.push((u64::from(exif), IfdKind::Exif, false));
                    }
                }
                0x9003 => ifd.date_time_original = entry.ascii(r, le)?,
                0x9004 => ifd.date_time_digitized = entry.ascii(r, le)?,
                0x9011 => ifd.offset_time_original = entry.ascii(r, le)?,
                0xA002 => ifd.pixel_x = entry.uint(r, le)?,
                0xA003 => ifd.pixel_y = entry.uint(r, le)?,
                0xC612 => ifd.dng_version = true,
                _ => {}
            }
        }

        if let (Some(offs), Some(lens)) = (&strip_offsets, &strip_counts) {
            if let (Some(&o), Some(&l)) = (offs.first(), lens.first()) {
                ifd.single_strip = Some((u64::from(o), u64::from(l)));
            }
        }
        if let (Some(o), Some(l)) = (jpeg_off, jpeg_len) {
            ifd.jpeg_range = Some((u64::from(o), u64::from(l)));
        }
        ifds.push(ifd);

        if follow_chain {
            let next = u64::from(u32_from(&table[12 * n..12 * n + 4], le));
            if next != 0 {
                queue.push((next, IfdKind::Primary, true));
            }
        }
    }

    Ok(TiffWalk { magic, ifds })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    type Put16 = fn(u16) -> [u8; 2];
    type Put32 = fn(u32) -> [u8; 4];

    /// Builds a minimal single-IFD TIFF in memory.
    fn tiny_tiff(le: bool) -> Vec<u8> {
        let mut b: Vec<u8> = Vec::new();
        let (order, put16, put32): (&[u8; 2], Put16, Put32) = if le {
            (b"II", u16::to_le_bytes, u32::to_le_bytes)
        } else {
            (b"MM", u16::to_be_bytes, u32::to_be_bytes)
        };
        b.extend_from_slice(order);
        b.extend_from_slice(&put16(42));
        b.extend_from_slice(&put32(8)); // IFD0 at 8
        b.extend_from_slice(&put16(3)); // 3 entries
                                        // ImageWidth = 640 (SHORT)
        b.extend_from_slice(&put16(0x0100));
        b.extend_from_slice(&put16(3));
        b.extend_from_slice(&put32(1));
        b.extend_from_slice(&put16(640));
        b.extend_from_slice(&put16(0));
        // ImageLength = 480 (LONG)
        b.extend_from_slice(&put16(0x0101));
        b.extend_from_slice(&put16(4));
        b.extend_from_slice(&put32(1));
        b.extend_from_slice(&put32(480));
        // Orientation = 6
        b.extend_from_slice(&put16(0x0112));
        b.extend_from_slice(&put16(3));
        b.extend_from_slice(&put32(1));
        b.extend_from_slice(&put16(6));
        b.extend_from_slice(&put16(0));
        b.extend_from_slice(&put32(0)); // no next IFD
        b
    }

    #[test]
    fn walks_both_byte_orders() {
        for le in [true, false] {
            let data = tiny_tiff(le);
            let walk = walk(&mut Cursor::new(&data)).unwrap();
            assert_eq!(walk.magic, 42);
            assert_eq!(walk.ifds.len(), 1);
            let ifd = &walk.ifds[0];
            assert_eq!(ifd.width, Some(640));
            assert_eq!(ifd.height, Some(480));
            assert_eq!(ifd.orientation, Some(6));
        }
    }

    #[test]
    fn cyclic_next_pointer_terminates() {
        let mut data = tiny_tiff(true);
        // Point "next IFD" back at IFD0.
        let len = data.len();
        data[len - 4..].copy_from_slice(&8u32.to_le_bytes());
        let walk = walk(&mut Cursor::new(&data)).unwrap();
        assert_eq!(walk.ifds.len(), 1, "cycle must not loop");
    }

    #[test]
    fn truncated_ifd_is_malformed_not_panic() {
        let data = tiny_tiff(true);
        for cut in [3, 9, 11, 20] {
            let out = walk(&mut Cursor::new(&data[..cut]));
            assert!(
                matches!(out, Err(ProbeError::Malformed(_))),
                "cut at {cut}: {out:?}"
            );
        }
    }

    #[test]
    fn absurd_entry_count_is_malformed() {
        let mut data = tiny_tiff(true);
        data[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(matches!(
            walk(&mut Cursor::new(&data)),
            Err(ProbeError::Malformed(_))
        ));
    }

    #[test]
    fn non_tiff_is_malformed() {
        assert!(matches!(
            walk(&mut Cursor::new(b"garbage!")),
            Err(ProbeError::Malformed(_))
        ));
        // Right order marker, wrong magic.
        assert!(matches!(
            walk(&mut Cursor::new(b"II\x99\x99\x08\x00\x00\x00")),
            Err(ProbeError::Malformed(_))
        ));
    }
}
