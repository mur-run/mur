//! The ELF program interpreter (`PT_INTERP`, e.g. `ld-linux-x86-64.so.2`) of
//! an executable.
//!
//! Landlock checks `Execute` on the interpreter too: the kernel opens it with
//! `open_exec`, which carries `__FMODE_EXEC`. Under strict spawn mode the
//! read grants over `/lib`, `/lib64` and `/usr/lib` no longer carry `Execute`
//! (#2343), so each allowlisted binary's interpreter must be granted on its
//! own or every dynamically linked allowlisted binary fails to start.
//!
//! Pure parsing, compiled everywhere so it is tested on every host.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS32: u8 = 1;
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ELFDATA2MSB: u8 = 2;
const PT_INTERP: u32 = 3;
/// Longest interpreter path accepted; a real one is a few dozen bytes.
const MAX_INTERP_LEN: u64 = 4096;
/// Most program headers walked before giving up on a malformed file.
const MAX_PHNUM: u16 = 512;

/// The interpreter `path` names, or `None` for a static binary, a script, a
/// non-ELF file, or anything unreadable or malformed. Never errors: a missing
/// interpreter grant fails closed (the binary will not start), it never widens.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn elf_interpreter(path: &Path) -> Option<PathBuf> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut ident = [0u8; 16];
    f.read_exact(&mut ident).ok()?;
    if ident[..4] != ELF_MAGIC {
        return None;
    }
    let wide = match ident[4] {
        ELFCLASS64 => true,
        ELFCLASS32 => false,
        _ => return None,
    };
    let little = match ident[5] {
        ELFDATA2LSB => true,
        ELFDATA2MSB => false,
        _ => return None,
    };
    let rd = Reader { little };

    // e_phoff, e_phentsize, e_phnum sit at fixed offsets per class.
    let (phoff, phentsize, phnum) = if wide {
        let mut h = [0u8; 48];
        f.read_exact(&mut h).ok()?; // bytes 16..64
        (rd.u64(&h[16..24]), rd.u16(&h[38..40]), rd.u16(&h[40..42]))
    } else {
        let mut h = [0u8; 36];
        f.read_exact(&mut h).ok()?; // bytes 16..52
        (
            u64::from(rd.u32(&h[12..16])),
            rd.u16(&h[26..28]),
            rd.u16(&h[28..30]),
        )
    };
    if phnum > MAX_PHNUM {
        return None;
    }

    let mut ph = vec![0u8; usize::from(phentsize)];
    for i in 0..u64::from(phnum) {
        f.seek(SeekFrom::Start(
            phoff.checked_add(i * u64::from(phentsize))?,
        ))
        .ok()?;
        f.read_exact(&mut ph).ok()?;
        if rd.u32(ph.get(0..4)?) != PT_INTERP {
            continue;
        }
        let (offset, size) = if wide {
            (rd.u64(ph.get(8..16)?), rd.u64(ph.get(32..40)?))
        } else {
            (
                u64::from(rd.u32(ph.get(4..8)?)),
                u64::from(rd.u32(ph.get(16..20)?)),
            )
        };
        if size == 0 || size > MAX_INTERP_LEN {
            return None;
        }
        let mut raw = vec![0u8; usize::try_from(size).ok()?];
        f.seek(SeekFrom::Start(offset)).ok()?;
        f.read_exact(&mut raw).ok()?;
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        let s = std::str::from_utf8(&raw[..end]).ok()?;
        let p = PathBuf::from(s);
        return p.is_absolute().then_some(p);
    }
    None
}

struct Reader {
    little: bool,
}

impl Reader {
    fn u16(&self, b: &[u8]) -> u16 {
        let a = [b[0], b[1]];
        if self.little {
            u16::from_le_bytes(a)
        } else {
            u16::from_be_bytes(a)
        }
    }
    fn u32(&self, b: &[u8]) -> u32 {
        let a = [b[0], b[1], b[2], b[3]];
        if self.little {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        }
    }
    fn u64(&self, b: &[u8]) -> u64 {
        let a: [u8; 8] = b[..8].try_into().expect("8 bytes");
        if self.little {
            u64::from_le_bytes(a)
        } else {
            u64::from_be_bytes(a)
        }
    }
}

#[cfg(test)]
#[path = "elf_interp_tests.rs"]
mod tests;
