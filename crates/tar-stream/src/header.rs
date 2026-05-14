//! USTAR (POSIX 1003.1-1988) header generation.
//!
//! Why USTAR rather than GNU/PAX:
//!   * toybox tar (Phase 0 retro §1.2) reads USTAR + a few GNU extensions
//!     reliably. PAX extended headers are not guaranteed across older
//!     vendor tars.
//!   * We need name+prefix to handle paths up to 255 bytes, which USTAR
//!     gives us natively.
//!
//! If a path exceeds the USTAR 100/155 limit, we emit a **GNU `LongLink`
//! (`L`) extension header** carrying the full path, then a fallback
//! USTAR header. toybox supports this format.

use crate::error::{Result, TarError};
use crate::path::TarPath;

pub const BLOCK_SIZE: usize = 512;

/// Pad `len` up to the next 512-byte tar block boundary.
pub fn pad_to_block(len: u64) -> u64 {
    let r = len % BLOCK_SIZE as u64;
    if r == 0 {
        0
    } else {
        BLOCK_SIZE as u64 - r
    }
}

/// Build a header for a regular file. `path` is the validated tar entry
/// path; `size` is the payload in bytes; `mtime` is epoch seconds; `mode`
/// is the unix mode bits (we use 0o644 for files / 0o755 for dirs).
pub fn file_header(path: &TarPath, size: u64, mtime: i64, mode: u32) -> Result<Vec<u8>> {
    header(path, size, mtime, mode, b'0')
}

/// Build a directory header. Size is always 0; trailing `/` is recommended
/// but not required by USTAR — we add it because some readers key on it.
pub fn dir_header(path: &TarPath, mtime: i64) -> Result<Vec<u8>> {
    header(path, 0, mtime, 0o755, b'5')
}

fn header(path: &TarPath, size: u64, mtime: i64, mode: u32, typeflag: u8) -> Result<Vec<u8>> {
    let full = path.as_str();
    // Tar dir entries conventionally end with '/'. Add only if missing
    // and only for directories.
    let full = if typeflag == b'5' && !full.ends_with('/') {
        format!("{full}/")
    } else {
        full
    };

    let (name, prefix) = split_for_ustar(&full)?;

    // For paths that don't fit USTAR's 100/155 split we prepend a GNU
    // `L` extension carrying the full path. The header itself still gets
    // a truncated copy (we use a deterministic hash-free truncation:
    // last 99 bytes) so older readers that ignore the `L` block at
    // least see something semi-meaningful.
    if name.is_empty() {
        // we couldn't split — emit LongLink + truncated header
        let mut out = Vec::with_capacity(BLOCK_SIZE * 3 + full.len());
        out.extend_from_slice(&gnu_longlink(&full, b'L')?);
        let truncated = truncate_for_fallback(&full);
        out.extend_from_slice(&one_header(&truncated, "", size, mtime, mode, typeflag)?);
        return Ok(out);
    }

    one_header(name, prefix, size, mtime, mode, typeflag)
}

fn one_header(
    name: &str,
    prefix: &str,
    size: u64,
    mtime: i64,
    mode: u32,
    typeflag: u8,
) -> Result<Vec<u8>> {
    let mut block = [0u8; BLOCK_SIZE];
    write_str(&mut block[0..100], name)?;
    write_octal(&mut block[100..108], mode as u64, 7);
    write_octal(&mut block[108..116], 0, 7); // uid 0 — we don't set owner per plan.md §1.2
    write_octal(&mut block[116..124], 0, 7); // gid 0
    write_octal(&mut block[124..136], size, 11);
    let mtime_u = if mtime < 0 { 0 } else { mtime as u64 };
    write_octal(&mut block[136..148], mtime_u, 11);
    // checksum field: filled with spaces during calculation
    for b in &mut block[148..156] {
        *b = b' ';
    }
    block[156] = typeflag;
    // linkname [157..257] left zero
    // ustar magic
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    // uname/gname left empty
    // devmajor/devminor zero
    write_str(&mut block[345..500], prefix)?;
    // compute checksum
    let sum: u32 = block.iter().map(|b| *b as u32).sum();
    write_octal(&mut block[148..155], sum as u64, 6);
    block[155] = b' ';

    Ok(block.to_vec())
}

/// GNU `LongLink` block (typeflag 'L' for path, 'K' for linkname).
/// Layout: header block with size = len(value)+1 then value+NUL padded
/// to 512 bytes.
fn gnu_longlink(value: &str, typeflag: u8) -> Result<Vec<u8>> {
    let bytes = value.as_bytes();
    let mut block = [0u8; BLOCK_SIZE];
    write_str(&mut block[0..100], "././@LongLink")?;
    write_octal(&mut block[100..108], 0, 7);
    write_octal(&mut block[108..116], 0, 7);
    write_octal(&mut block[116..124], 0, 7);
    write_octal(&mut block[124..136], (bytes.len() + 1) as u64, 11);
    write_octal(&mut block[136..148], 0, 11);
    for b in &mut block[148..156] {
        *b = b' ';
    }
    block[156] = typeflag;
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    let sum: u32 = block.iter().map(|b| *b as u32).sum();
    write_octal(&mut block[148..155], sum as u64, 6);
    block[155] = b' ';

    let mut out = Vec::with_capacity(BLOCK_SIZE * 2 + bytes.len());
    out.extend_from_slice(&block);
    out.extend_from_slice(bytes);
    out.push(0);
    let pad = pad_to_block((bytes.len() + 1) as u64);
    out.extend(std::iter::repeat(0u8).take(pad as usize));
    Ok(out)
}

fn truncate_for_fallback(full: &str) -> String {
    let bytes = full.as_bytes();
    if bytes.len() <= 99 {
        return full.to_string();
    }
    // Keep last 99 bytes, but step forward to the next valid utf-8 boundary.
    let mut start = bytes.len() - 99;
    while start < bytes.len() && (bytes[start] & 0b1100_0000) == 0b1000_0000 {
        start += 1;
    }
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

fn write_str(slot: &mut [u8], s: &str) -> Result<()> {
    let bytes = s.as_bytes();
    if bytes.len() > slot.len() {
        return Err(TarError::InvalidEntryName {
            reason: format!(
                "string exceeds tar header slot of {} bytes: {s}",
                slot.len()
            ),
        });
    }
    for (i, b) in bytes.iter().enumerate() {
        slot[i] = *b;
    }
    Ok(())
}

fn write_octal(slot: &mut [u8], v: u64, digits: usize) {
    let s = format!("{v:0digits$o}", digits = digits);
    let bytes = s.as_bytes();
    let off = slot.len() - bytes.len() - 1; // leave trailing NUL
    for (i, b) in bytes.iter().enumerate() {
        slot[off + i] = *b;
    }
}

/// Split a path into (name, prefix) for the USTAR header. Returns
/// `("", "")` when the path does NOT fit even after splitting — caller
/// emits a GNU LongLink instead.
fn split_for_ustar(full: &str) -> Result<(&str, &str)> {
    if full.as_bytes().len() <= 100 {
        return Ok((full, ""));
    }
    // Find a `/` such that everything after it is ≤ 100 bytes and
    // everything before it is ≤ 155 bytes. Prefer the split closest to
    // the end so the `name` slot stays meaningful.
    let bytes = full.as_bytes();
    let mut best: Option<usize> = None;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'/' {
            let after = bytes.len() - i - 1;
            let before = i;
            if after <= 100 && before <= 155 && after > 0 {
                best = Some(i);
            }
        }
    }
    match best {
        Some(i) => Ok((&full[i + 1..], &full[..i])),
        None => Ok(("", "")),
    }
}

/// Two zero blocks marking the end-of-archive.
pub fn end_of_archive_marker() -> Vec<u8> {
    vec![0u8; BLOCK_SIZE * 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tp(s: &str) -> TarPath {
        TarPath::new(s).unwrap()
    }

    #[test]
    fn file_header_is_one_block() {
        let h = file_header(&tp("hello.txt"), 5, 0, 0o644).unwrap();
        assert_eq!(h.len(), BLOCK_SIZE);
    }

    #[test]
    fn header_has_ustar_magic() {
        let h = file_header(&tp("a.txt"), 0, 0, 0o644).unwrap();
        assert_eq!(&h[257..263], b"ustar\0");
        assert_eq!(&h[263..265], b"00");
    }

    #[test]
    fn checksum_is_sum_of_bytes_with_spaces_in_chksum_slot() {
        let h = file_header(&tp("a.txt"), 0, 0, 0o644).unwrap();
        // recompute with chksum bytes replaced by spaces
        let mut buf = h.clone();
        for b in &mut buf[148..156] {
            *b = b' ';
        }
        let recomputed: u32 = buf.iter().map(|b| *b as u32).sum();
        // header has the value written; parse it as octal
        let stored = std::str::from_utf8(&h[148..155])
            .unwrap()
            .trim_matches('\0')
            .trim();
        let stored_v = u32::from_str_radix(stored, 8).unwrap();
        assert_eq!(stored_v, recomputed);
    }

    #[test]
    fn long_paths_use_split_into_name_and_prefix() {
        // 120-byte path: prefix should be populated, name <= 100.
        let p: String = "abc/".repeat(30) + "tail.txt";
        let h = file_header(&tp(&p), 0, 0, 0o644).unwrap();
        let name = std::str::from_utf8(&h[0..100])
            .unwrap()
            .trim_matches('\0');
        let prefix = std::str::from_utf8(&h[345..500])
            .unwrap()
            .trim_matches('\0');
        assert!(name.ends_with("tail.txt"));
        assert!(!prefix.is_empty());
        assert!(name.len() <= 100);
        assert!(prefix.len() <= 155);
    }

    #[test]
    fn very_long_paths_emit_longlink_block_then_fallback() {
        // 300-byte single-segment path: no `/`, can't be split.
        let p = "a".repeat(300);
        let h = file_header(&tp(&p), 0, 0, 0o644).unwrap();
        assert_eq!(h.len() % BLOCK_SIZE, 0);
        // First block = LongLink with typeflag 'L' at offset 156
        assert_eq!(h[156], b'L');
        // Magic should still be ustar
        assert_eq!(&h[257..263], b"ustar\0");
    }

    #[test]
    fn dir_header_has_typeflag_5_and_trailing_slash() {
        let h = dir_header(&tp("subdir"), 0).unwrap();
        assert_eq!(h[156], b'5');
        let name = std::str::from_utf8(&h[0..100])
            .unwrap()
            .trim_matches('\0');
        assert_eq!(name, "subdir/");
    }

    #[test]
    fn pad_to_block_round_trips() {
        assert_eq!(pad_to_block(0), 0);
        assert_eq!(pad_to_block(512), 0);
        assert_eq!(pad_to_block(1), 511);
        assert_eq!(pad_to_block(513), 511);
    }

    #[test]
    fn end_of_archive_is_two_zero_blocks() {
        let m = end_of_archive_marker();
        assert_eq!(m.len(), 1024);
        assert!(m.iter().all(|b| *b == 0));
    }
}
