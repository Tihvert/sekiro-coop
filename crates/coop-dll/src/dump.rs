//! RE helper (F10): write the running, SteamStub-decrypted sekiro.exe image to
//! disk so IDA/Ghidra can analyse real code (the on-disk `.text` is encrypted).
//!
//! The image is copied page by page (unreadable pages become zeros) and the
//! section headers are rewritten so file offsets equal virtual addresses.

use std::fs;
use std::path::PathBuf;

use crate::game::{module_base, read};
use crate::log;

const PAGE: usize = 0x1000;

fn out_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("sekiro-coop").join("sekiro_dump.exe")
}

pub fn dump_image() {
    let base = module_base();
    let Some(e_lfanew) = read::<u32>(base + 0x3C) else {
        log::line("dump: no PE header");
        return;
    };
    let nt = base + e_lfanew as usize;
    let (Some(size_of_image), Some(sections), Some(opt_size)) = (
        read::<u32>(nt + 0x18 + 0x38),
        read::<u16>(nt + 6),
        read::<u16>(nt + 0x14),
    ) else {
        log::line("dump: bad NT header");
        return;
    };

    let size = size_of_image as usize;
    let mut image = vec![0u8; size];
    let mut unreadable = 0;
    for off in (0..size).step_by(PAGE) {
        match read::<[u8; PAGE]>(base + off) {
            Some(page) => image[off..off + PAGE].copy_from_slice(&page),
            None => unreadable += 1,
        }
    }

    // Raw layout = virtual layout.
    let sec_table = e_lfanew as usize + 0x18 + opt_size as usize;
    for i in 0..sections as usize {
        let s = sec_table + i * 40;
        let vsize = u32::from_le_bytes(image[s + 8..s + 12].try_into().unwrap());
        let vaddr = u32::from_le_bytes(image[s + 12..s + 16].try_into().unwrap());
        image[s + 16..s + 20].copy_from_slice(&vsize.to_le_bytes()); // SizeOfRawData
        image[s + 20..s + 24].copy_from_slice(&vaddr.to_le_bytes()); // PointerToRawData
    }

    let path = out_path();
    match fs::write(&path, &image) {
        Ok(()) => log::line(&format!(
            "dump: wrote {} ({size:#x} bytes, base {base:#x}, {unreadable} unreadable pages)",
            path.display()
        )),
        Err(e) => log::line(&format!("dump: write failed: {e}")),
    }
}
