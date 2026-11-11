use super::*;

/// A minimal 64-bit little-endian ELF: header, one `PT_INTERP` program
/// header, then the interpreter string.
fn elf64_with_interp(interp: &str) -> Vec<u8> {
    let phoff: u64 = 64;
    let phentsize: u16 = 56;
    let interp_off: u64 = phoff + u64::from(phentsize);
    let mut b = vec![0u8; 64];
    b[..4].copy_from_slice(&ELF_MAGIC);
    b[4] = ELFCLASS64;
    b[5] = ELFDATA2LSB;
    b[32..40].copy_from_slice(&phoff.to_le_bytes());
    b[54..56].copy_from_slice(&phentsize.to_le_bytes());
    b[56..58].copy_from_slice(&1u16.to_le_bytes());
    let mut ph = vec![0u8; usize::from(phentsize)];
    ph[0..4].copy_from_slice(&PT_INTERP.to_le_bytes());
    ph[8..16].copy_from_slice(&interp_off.to_le_bytes());
    let size = interp.len() as u64 + 1;
    ph[32..40].copy_from_slice(&size.to_le_bytes());
    b.extend(ph);
    b.extend(interp.as_bytes());
    b.push(0);
    b
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

#[test]
fn reads_the_interpreter_of_a_dynamic_elf() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write(
        tmp.path(),
        "dyn",
        &elf64_with_interp("/lib64/ld-linux-x86-64.so.2"),
    );
    assert_eq!(
        elf_interpreter(&p),
        Some(PathBuf::from("/lib64/ld-linux-x86-64.so.2"))
    );
}

#[test]
fn a_script_a_static_elf_and_garbage_have_no_interpreter() {
    let tmp = tempfile::tempdir().unwrap();
    let script = write(tmp.path(), "s", b"#!/bin/sh\necho hi\n");
    let mut static_elf = elf64_with_interp("/x");
    static_elf[64..68].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD, not PT_INTERP
    let st = write(tmp.path(), "static", &static_elf);
    let short = write(tmp.path(), "short", &ELF_MAGIC);
    let relative = write(tmp.path(), "rel", &elf64_with_interp("ld.so"));
    for p in [&script, &st, &short, &relative] {
        assert_eq!(elf_interpreter(p), None, "{}", p.display());
    }
    assert_eq!(elf_interpreter(&tmp.path().join("missing")), None);
}

/// Negative control on a real binary: the test executable itself is a
/// dynamically linked ELF on Linux, so the parser must find its loader.
#[cfg(target_os = "linux")]
#[test]
fn the_test_binary_has_a_real_interpreter() {
    let exe = std::env::current_exe().unwrap();
    let interp = elf_interpreter(&exe).expect("dynamic test binary");
    assert!(interp.exists(), "{}", interp.display());
}
