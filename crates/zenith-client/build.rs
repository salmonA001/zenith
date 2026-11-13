// Gives the Windows game its icon, and its name in Explorer and Task Manager. A program keeps these as
// resources, which usually takes a resource compiler; this writes the object file such a compiler
// would, so that building still needs nothing but rustup.
use std::path::PathBuf;

include!("src/icon.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/icon.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // Each kind of machine marks a reference to a resource in its own way.
    let (machine, relative) = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => (0x8664u16, 3u16),
        Ok("aarch64") => (0xaa64, 2),
        Ok("x86") => (0x014c, 7),
        _ => return,
    };
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let mut resources = icon_resources();
    resources.push(Resource { kind: 16, id: 1, data: version_info(&version) });
    let (section, references) = resource_section(&mut resources);
    let object = coff(machine, relative, &section, &references);
    let Some(out) = std::env::var_os("OUT_DIR").map(PathBuf::from) else { return };
    let path = out.join("zenith-resources.o");
    match std::fs::write(&path, object) {
        Ok(()) => println!("cargo:rustc-link-arg-bin=zenith={}", path.display()),
        Err(e) => println!("cargo:warning=the game will have no icon: {e}"),
    }
}

struct Resource {
    kind: u16,
    id: u16,
    data: Vec<u8>,
}

/// The icon at the sizes Windows asks for, and the directory that lists them.
fn icon_resources() -> Vec<Resource> {
    let sizes = [16u32, 32, 48, 256];
    let mut resources: Vec<Resource> =
        sizes.iter().zip(1..).map(|(&s, id)| Resource { kind: 3, id, data: bitmap(&paint_icon(s), s) }).collect();
    let mut group = Vec::new();
    group.extend(0u16.to_le_bytes());
    group.extend(1u16.to_le_bytes());
    group.extend((sizes.len() as u16).to_le_bytes());
    for (s, r) in sizes.iter().zip(&resources) {
        // 256 does not fit in a byte, and is written as 0.
        let side = if *s >= 256 { 0 } else { *s as u8 };
        group.extend([side, side, 0, 0]);
        group.extend(1u16.to_le_bytes());
        group.extend(32u16.to_le_bytes());
        group.extend((r.data.len() as u32).to_le_bytes());
        group.extend(r.id.to_le_bytes());
    }
    resources.push(Resource { kind: 14, id: 1, data: group });
    resources
}

/// One icon image as Windows keeps it: a bitmap header, the pixels from the bottom row up in BGRA,
/// then a one-bit mask, left clear because the alpha already says what shows.
fn bitmap(rgba: &[u8], size: u32) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend(40u32.to_le_bytes());
    d.extend(size.to_le_bytes());
    d.extend((size * 2).to_le_bytes());
    d.extend(1u16.to_le_bytes());
    d.extend(32u16.to_le_bytes());
    d.extend(0u32.to_le_bytes());
    d.extend((size * size * 4).to_le_bytes());
    d.extend([0u8; 16]);
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            d.extend([rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    let mask_row = size.div_ceil(32) as usize * 4;
    d.resize(d.len() + mask_row * size as usize, 0);
    d
}

/// The version resource: the name, version and licence Windows shows for the program.
fn version_info(version: &str) -> Vec<u8> {
    let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor, patch) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    let (high, low) = ((major << 16) | minor, patch << 16);
    let fixed: Vec<u8> = [0xfeef04bd, 0x0001_0000, high, low, high, low, 0x3f, 0, 0x0004_0004, 1, 0, 0, 0]
        .iter()
        .flat_map(|w: &u32| w.to_le_bytes())
        .collect();
    let strings: Vec<Vec<u8>> = [
        ("CompanyName", "Zenith contributors"),
        ("FileDescription", "Zenith"),
        ("FileVersion", version),
        ("InternalName", "zenith"),
        ("LegalCopyright", "Zenith contributors, under the MIT licence"),
        ("OriginalFilename", "zenith.exe"),
        ("ProductName", "Zenith"),
        ("ProductVersion", version),
    ]
    .iter()
    .map(|(key, value)| node(key, &utf16(value), true, &[]))
    .collect();
    let table = node("040904B0", &[], true, &strings);
    let string_info = node("StringFileInfo", &[], true, &[table]);
    // English (United States), in Unicode.
    let translation = node("Translation", &[0x09, 0x04, 0xb0, 0x04], false, &[]);
    let var_info = node("VarFileInfo", &[], true, &[translation]);
    node("VS_VERSION_INFO", &fixed, false, &[string_info, var_info])
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

fn pad(b: &mut Vec<u8>) {
    b.resize(b.len().div_ceil(4) * 4, 0);
}

/// One block of a version resource: its length, its value's length, whether that value is text,
/// its name, its value, then the blocks inside it, each starting on a four-byte boundary.
fn node(key: &str, value: &[u8], text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![0u8; 6];
    b.extend(utf16(key));
    pad(&mut b);
    b.extend(value);
    for child in children {
        pad(&mut b);
        b.extend(child);
    }
    let value_length = if text { value.len() / 2 } else { value.len() };
    let length = b.len() as u16;
    b[0..2].copy_from_slice(&length.to_le_bytes());
    b[2..4].copy_from_slice(&(value_length as u16).to_le_bytes());
    b[4..6].copy_from_slice(&u16::from(text).to_le_bytes());
    b
}

fn put(out: &mut [u8], at: usize, value: u32) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// Writes a directory of the resource tree: a header, then (name, where) pairs.
fn directory(out: &mut [u8], at: usize, entries: &[(u32, u32)]) {
    out[at + 14..at + 16].copy_from_slice(&(entries.len() as u16).to_le_bytes());
    for (i, (name, target)) in entries.iter().enumerate() {
        put(out, at + 16 + i * 8, *name);
        put(out, at + 20 + i * 8, *target);
    }
}

/// The `.rsrc` section: a tree of kind, then number, then language, ending in entries that say
/// where each resource's bytes are. Those positions are addresses in the finished program, so the
/// linker fills them in: the second list says where they are.
fn resource_section(resources: &mut [Resource]) -> (Vec<u8>, Vec<u32>) {
    const SUBDIRECTORY: u32 = 0x8000_0000;
    const ENGLISH: u32 = 0x0409;
    resources.sort_by_key(|r| (r.kind, r.id));
    let mut kinds: Vec<u16> = resources.iter().map(|r| r.kind).collect();
    kinds.dedup();
    let size = |entries: usize| 16 + entries * 8;
    let mut at = size(kinds.len());
    let mut kind_at = Vec::new();
    for &k in &kinds {
        kind_at.push(at);
        at += size(resources.iter().filter(|r| r.kind == k).count());
    }
    let (mut language_at, mut entry_at, mut data_at) = (Vec::new(), Vec::new(), Vec::new());
    for _ in resources.iter() {
        language_at.push(at);
        at += size(1);
    }
    for _ in resources.iter() {
        entry_at.push(at);
        at += 16;
    }
    for r in resources.iter() {
        at = at.div_ceil(8) * 8;
        data_at.push(at);
        at += r.data.len();
    }

    let mut out = vec![0u8; at];
    let top: Vec<(u32, u32)> =
        kinds.iter().zip(&kind_at).map(|(&k, &a)| (u32::from(k), a as u32 | SUBDIRECTORY)).collect();
    directory(&mut out, 0, &top);
    for (&k, &a) in kinds.iter().zip(&kind_at) {
        let named: Vec<(u32, u32)> = resources
            .iter()
            .zip(&language_at)
            .filter(|(r, _)| r.kind == k)
            .map(|(r, &l)| (u32::from(r.id), l as u32 | SUBDIRECTORY))
            .collect();
        directory(&mut out, a, &named);
    }
    for (i, r) in resources.iter().enumerate() {
        directory(&mut out, language_at[i], &[(ENGLISH, entry_at[i] as u32)]);
        put(&mut out, entry_at[i], data_at[i] as u32);
        put(&mut out, entry_at[i] + 4, r.data.len() as u32);
        out[data_at[i]..data_at[i] + r.data.len()].copy_from_slice(&r.data);
    }
    (out, entry_at.iter().map(|&e| e as u32).collect())
}

/// A COFF object file holding one section, its references, and the one symbol they are relative to.
fn coff(machine: u16, relative: u16, section: &[u8], references: &[u32]) -> Vec<u8> {
    let data_at = 20 + 40;
    let references_at = data_at + section.len();
    let symbols_at = references_at + references.len() * 10;
    let name = *b".rsrc\0\0\0";
    let mut out = Vec::new();
    out.extend(machine.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend((symbols_at as u32).to_le_bytes());
    out.extend(2u32.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend(0u16.to_le_bytes());

    out.extend(name);
    out.extend(0u32.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend((section.len() as u32).to_le_bytes());
    out.extend((data_at as u32).to_le_bytes());
    out.extend((references_at as u32).to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend((references.len() as u16).to_le_bytes());
    out.extend(0u16.to_le_bytes());
    // Initialised data, readable, on an eight-byte boundary.
    out.extend(0x4040_0040u32.to_le_bytes());

    out.extend(section);
    for &at in references {
        out.extend(at.to_le_bytes());
        out.extend(0u32.to_le_bytes());
        out.extend(relative.to_le_bytes());
    }

    // The section's own symbol, with the record that describes the section.
    out.extend(name);
    out.extend(0u32.to_le_bytes());
    out.extend(1i16.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend([3, 1]);
    out.extend((section.len() as u32).to_le_bytes());
    out.extend((references.len() as u16).to_le_bytes());
    out.extend([0u8; 12]);
    // No long names: the string table is only its own length.
    out.extend(4u32.to_le_bytes());
    out
}
