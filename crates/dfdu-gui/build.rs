//! Gives `dfdu-gui.exe` dfdu's icon on Windows (MSVC), so Explorer, the taskbar and the Start
//! menu shortcut the installer makes show the mark rather than the default program icon.
//!
//! The icon (`windows/dfdu.ico`) is written into a compiled resource file (`.res`) here, which
//! the MSVC linker takes as an input like an object file. Writing the few headers by hand keeps
//! the build free of a resource compiler and of a build dependency; other targets do nothing.

use std::path::{Path, PathBuf};
use std::{env, fs};

/// Resource type of one image of an icon.
const RT_ICON: u16 = 3;
/// Resource type of the directory of an icon's images; Explorer takes the lowest-numbered one.
const RT_GROUP_ICON: u16 = 14;
/// Memory flags `rc.exe` gives icons: moveable and discardable, and pure for the directory.
const ICON_FLAGS: u16 = 0x1010;
const GROUP_FLAGS: u16 = 0x1030;
/// English (United States), `rc.exe`'s default language.
const LANGUAGE: u16 = 0x0409;

fn main() {
    let ico = Path::new("windows/dfdu.ico");
    println!("cargo::rerun-if-changed={}", ico.display());
    let windows = env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    let msvc = env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|e| e == "msvc");
    if !(windows && msvc) {
        return;
    }
    let ico = fs::read(ico).expect("read windows/dfdu.ico");
    let res = icon_res(&ico).expect("windows/dfdu.ico is not an icon file");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("dfdu.res");
    fs::write(&out, res).expect("write dfdu.res");
    println!("cargo::rustc-link-arg-bins={}", out.display());
}

/// The `.res` file of an `.ico` file: one `RT_ICON` per image (numbered from 1) and an
/// `RT_GROUP_ICON` numbered 1 listing them, as `rc.exe` compiles `1 ICON "dfdu.ico"`.
fn icon_res(ico: &[u8]) -> Option<Vec<u8>> {
    let word = |at: usize| Some(u16::from_le_bytes(ico.get(at..at + 2)?.try_into().ok()?));
    let dword = |at: usize| Some(u32::from_le_bytes(ico.get(at..at + 4)?.try_into().ok()?));
    if word(0)? != 0 || word(2)? != 1 {
        return None;
    }
    let count = word(4)?;
    // A `.res` file opens with an empty resource that marks it as 32-bit.
    let mut res = Vec::new();
    entry(&mut res, 0, 0, 0, 0, &[]);
    let mut group = Vec::new();
    group.extend_from_slice(&[0, 0, 1, 0]);
    group.extend_from_slice(&count.to_le_bytes());
    for i in 0..count {
        // ICONDIRENTRY: width, height, colours, reserved, planes, bits, size, offset (16 bytes).
        let at = 6 + 16 * usize::from(i);
        let size = dword(at + 8)?;
        let offset = usize::try_from(dword(at + 12)?).ok()?;
        let image = ico.get(offset..offset + usize::try_from(size).ok()?)?;
        let id = i + 1;
        entry(&mut res, RT_ICON, id, ICON_FLAGS, LANGUAGE, image);
        // GRPICONDIRENTRY: the same first 12 bytes, then the image's resource number.
        group.extend_from_slice(ico.get(at..at + 12)?);
        group.extend_from_slice(&id.to_le_bytes());
    }
    entry(&mut res, RT_GROUP_ICON, 1, GROUP_FLAGS, LANGUAGE, &group);
    Some(res)
}

/// Appends one resource: its header (type and name as numbers), its data, and padding to four
/// bytes.
fn entry(res: &mut Vec<u8>, kind: u16, name: u16, flags: u16, language: u16, data: &[u8]) {
    let size = u32::try_from(data.len()).expect("icon image under 4 GiB");
    res.extend_from_slice(&size.to_le_bytes());
    res.extend_from_slice(&32u32.to_le_bytes()); // header size
    res.extend_from_slice(&[0xff, 0xff]);
    res.extend_from_slice(&kind.to_le_bytes());
    res.extend_from_slice(&[0xff, 0xff]);
    res.extend_from_slice(&name.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes()); // data version
    res.extend_from_slice(&flags.to_le_bytes());
    res.extend_from_slice(&language.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes()); // version
    res.extend_from_slice(&0u32.to_le_bytes()); // characteristics
    res.extend_from_slice(data);
    res.resize(res.len().next_multiple_of(4), 0);
}
