// Two things ride along with the executable and neither can be expressed in
// Rust: the application manifest and the icon.
//
// The manifest is not decoration here. `SetLayeredWindowAttributes` on a
// *child* window — the M1b overlay scrollbar — is silently disabled for an
// image whose manifest does not declare Windows 8 or newer under
// `<supportedOS>`: a manifest-less PE is treated as version-Unknown, and the
// layered-child code path is never taken: without the embedded manifest the
// layered child fails to be created, and the ex-style never reaches it.
//
// The icon is written here rather than handed to a compiler tool so that a
// build needs nothing beyond the linker: `.res` is a flat sequence of
// length-prefixed records, and an icon group is the `.ico` directory with the
// file offsets swapped for ids — one group record naming one record per image.
fn main() {
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=assets/app.ico");

    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") || !target.contains("msvc") {
        return;
    }
    // Absolute, because the linker's working directory is not this crate.
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let manifest = std::path::Path::new(&dir).join("app.manifest");
    let manifest = manifest.display();
    for scope in ["bins", "examples"] {
        println!("cargo:rustc-link-arg-{scope}=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-{scope}=/MANIFESTINPUT:{manifest}");
    }

    let ico = std::path::Path::new(&dir).join("assets/app.ico");
    let res = icon_resources(&ico).unwrap_or_else(|e| panic!("{}: {e}", ico.display()));
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("app.res");
    std::fs::write(&out, res).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
    // Only the binaries get the icon: an example that opens a window is a
    // probe, not something anyone sees in a folder or on the taskbar.
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

/// The two kinds a record can be, as the linker numbers them: a single image,
/// and the directory that names the images making up one icon.
const ICON_IMAGE: u16 = 3;
const ICON_GROUP: u16 = 14;
/// `MOVEABLE | DISCARDABLE`, plus `PURE` for the group: the flags a resource
/// compiler writes, kept as-is so the image is indistinguishable from one it
/// produced.
const MEM_ICON: u16 = 0x1010;
const MEM_GROUP: u16 = 0x1030;
/// en-US, which is what a resource compiler defaults to. The shell falls
/// back to any language for an icon, so this only decides which copy wins if a
/// later build ever adds a second one.
const LANG: u16 = 0x0409;

/// One image inside the `.ico`, with the directory fields the group record
/// repeats verbatim.
struct Image<'a> {
    width: u8,
    height: u8,
    colors: u8,
    planes: u16,
    bit_count: u16,
    data: &'a [u8],
}

fn icon_resources(ico: &std::path::Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(ico).map_err(|e| e.to_string())?;
    let images = parse_ico(&bytes)?;

    let mut res = Vec::new();
    // The file opens with an empty record — type 0, name 0, no data — which is
    // what a resource compiler writes and what `cvtres` expects to see before
    // the first real one.
    res.extend_from_slice(&0u32.to_le_bytes()); // data size
    res.extend_from_slice(&32u32.to_le_bytes()); // header size
    res.extend_from_slice(&[0xFF, 0xFF, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00]);
    res.extend_from_slice(&[0u8; 16]);
    for (i, image) in images.iter().enumerate() {
        // Ids start at 1, so the group below can name them.
        push_record(&mut res, ICON_IMAGE, (i + 1) as u16, MEM_ICON, image.data);
    }

    let mut group = Vec::with_capacity(6 + 14 * images.len());
    group.extend_from_slice(&0u16.to_le_bytes()); // reserved
    group.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    group.extend_from_slice(&(images.len() as u16).to_le_bytes());
    for (i, image) in images.iter().enumerate() {
        group.push(image.width);
        group.push(image.height);
        group.push(image.colors);
        group.push(0); // reserved
        group.extend_from_slice(&image.planes.to_le_bytes());
        group.extend_from_slice(&image.bit_count.to_le_bytes());
        group.extend_from_slice(&(image.data.len() as u32).to_le_bytes());
        group.extend_from_slice(&((i + 1) as u16).to_le_bytes());
    }
    // Id 1 is what the frame's `LoadImageW` asks for.
    push_record(&mut res, ICON_GROUP, 1, MEM_GROUP, &group);
    Ok(res)
}

fn parse_ico(bytes: &[u8]) -> Result<Vec<Image<'_>>, String> {
    if bytes.len() < 6 {
        return Err("shorter than an ICONDIR".into());
    }
    let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let u32_at =
        |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    if u16_at(0) != 0 || u16_at(2) != 1 {
        return Err("not an icon (ICONDIR reserved/type)".into());
    }
    let count = u16_at(4) as usize;
    if bytes.len() < 6 + 16 * count {
        return Err("directory truncated".into());
    }
    let mut images = Vec::with_capacity(count);
    for i in 0..count {
        let at = 6 + 16 * i;
        let size = u32_at(at + 8) as usize;
        let offset = u32_at(at + 12) as usize;
        let data = bytes
            .get(offset..offset + size)
            .ok_or_else(|| format!("image {i} outside the file"))?;
        images.push(Image {
            width: bytes[at],
            height: bytes[at + 1],
            colors: bytes[at + 2],
            planes: u16_at(at + 4),
            bit_count: u16_at(at + 6),
            data,
        });
    }
    Ok(images)
}

/// Append one `.res` record: a fixed 32-byte header — the two lengths, the
/// type and name as ordinals, then five bookkeeping words — followed by the
/// data, padded to a DWORD boundary.
fn push_record(out: &mut Vec<u8>, ty: u16, name: u16, flags: u16, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    // Size of everything up to the data, i.e. the data's offset from the start
    // of this record: 4 + 4 + 4 + 4 + 16.
    out.extend_from_slice(&32u32.to_le_bytes());
    out.extend_from_slice(&0xFFFFu16.to_le_bytes()); // type is an ordinal
    out.extend_from_slice(&ty.to_le_bytes());
    out.extend_from_slice(&0xFFFFu16.to_le_bytes()); // name is an ordinal
    out.extend_from_slice(&name.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // data version
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&LANG.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // version
    out.extend_from_slice(&0u32.to_le_bytes()); // characteristics
    out.extend_from_slice(data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}
