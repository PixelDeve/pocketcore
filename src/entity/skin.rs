//! Procedurally generated 64x32 RGBA skins for bots and NPCs (8192 bytes,
//! the size a 0.14 client accepts), so fake players need no asset files.

pub const SKIN_BYTES: usize = 64 * 32 * 4;

fn put(buf: &mut [u8], x0: usize, y0: usize, w: usize, h: usize, rgb: [u8; 3]) {
    for y in y0..y0 + h {
        for x in x0..x0 + w {
            let i = (y * 64 + x) * 4;
            buf[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
}

/// Classic skin-sheet layout: head (0..32 x 0..16), legs (0..16 x 16..32),
/// torso (16..40 x 16..32), arms (40..56 x 16..32).
pub fn blocky(skin: [u8; 3], hair: [u8; 3], shirt: [u8; 3], pants: [u8; 3]) -> Vec<u8> {
    let mut b = vec![0u8; SKIN_BYTES];
    put(&mut b, 0, 0, 32, 16, skin);
    put(&mut b, 8, 0, 16, 8, hair); // top and bottom of the head
    put(&mut b, 0, 8, 32, 3, hair); // hair line around the head
    put(&mut b, 0, 16, 16, 16, pants); // legs
    put(&mut b, 16, 16, 24, 16, shirt); // torso
    put(&mut b, 40, 16, 16, 16, shirt); // arms
    b
}

/// Bot skin tinted with the team's colour.
pub fn team_skin(team: usize) -> Vec<u8> {
    let shirt = [[200, 40, 40], [60, 180, 60], [220, 200, 40], [50, 80, 200]][team % 4];
    let pants = [[120, 20, 20], [30, 110, 30], [140, 120, 20], [25, 40, 130]][team % 4];
    blocky([224, 172, 130], [70, 45, 25], shirt, pants)
}

/// Shop keeper: green robe on a brown face.
pub fn npc_skin() -> Vec<u8> {
    blocky([190, 140, 100], [60, 40, 30], [40, 120, 60], [30, 80, 45])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_and_opacity() {
        for s in [npc_skin(), team_skin(0), team_skin(3)] {
            assert_eq!(s.len(), 8192);
            assert!(s.chunks(4).take(16).all(|p| p[3] == 255)); // top head row opaque
        }
        assert_ne!(team_skin(0), team_skin(1));
    }
}

// ---------------------------------------------------------------------------
// PNG skins: skins/bots/*.png (any number, bots cycle through them) and skins/shop.png
// ---------------------------------------------------------------------------

use std::path::Path;

fn be32(b: &[u8]) -> usize {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (a as i16, b as i16, c as i16);
    let p = ia + ib - ic;
    let (pa, pb, pc) = ((p - ia).abs(), (p - ib).abs(), (p - ic).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Minimal PNG decoder (no interlacing): returns width, height and RGBA bytes.
pub fn decode_png(data: &[u8]) -> Result<(usize, usize, Vec<u8>), String> {
    const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if data.len() < 8 || data[..8] != SIG {
        return Err("not a PNG file".into());
    }
    let (mut w, mut h, mut depth, mut ctype, mut interlace) = (0usize, 0usize, 0u8, 0u8, 0u8);
    let (mut idat, mut plte, mut trns) = (Vec::new(), Vec::new(), Vec::new());
    let mut pos = 8;
    while pos + 8 <= data.len() {
        let len = be32(&data[pos..pos + 4]);
        let kind = &data[pos + 4..pos + 8];
        let start = pos + 8;
        let end = start.checked_add(len).filter(|e| *e <= data.len()).ok_or("PNG chunk is truncated")?;
        let body = &data[start..end];
        match kind {
            b"IHDR" if body.len() >= 13 => {
                w = be32(&body[0..4]);
                h = be32(&body[4..8]);
                depth = body[8];
                ctype = body[9];
                interlace = body[12];
            }
            b"PLTE" => plte = body.to_vec(),
            b"tRNS" => trns = body.to_vec(),
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        pos = end + 4; // skip the CRC
    }
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return Err(format!("bad PNG size {w}x{h}"));
    }
    if interlace != 0 {
        return Err("interlaced PNG is not supported, re-save it without interlacing".into());
    }
    let channels = match ctype {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return Err(format!("unsupported PNG colour type {ctype}")),
    };
    let depth_ok = match ctype {
        3 => matches!(depth, 1 | 2 | 4 | 8),
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        _ => matches!(depth, 8 | 16),
    };
    if !depth_ok {
        return Err(format!("unsupported PNG bit depth {depth}"));
    }
    if idat.len() < 6 {
        return Err("PNG has no image data".into());
    }
    let raw = crate::world::leveldb::inflate(&idat[2..]).map_err(|e| format!("PNG data is corrupt: {e}"))?;
    let bits = channels * depth as usize;
    let stride = (w * bits + 7) / 8;
    let bpp = (bits / 8).max(1);
    if raw.len() < (stride + 1) * h {
        return Err("PNG image data is too short".into());
    }
    let mut img = vec![0u8; stride * h];
    for y in 0..h {
        let ft = raw[y * (stride + 1)];
        let line = &raw[y * (stride + 1) + 1..(y + 1) * (stride + 1)];
        for x in 0..stride {
            let a = if x >= bpp { img[y * stride + x - bpp] } else { 0 };
            let b = if y > 0 { img[(y - 1) * stride + x] } else { 0 };
            let c = if x >= bpp && y > 0 { img[(y - 1) * stride + x - bpp] } else { 0 };
            let v = match ft {
                0 => line[x],
                1 => line[x].wrapping_add(a),
                2 => line[x].wrapping_add(b),
                3 => line[x].wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => line[x].wrapping_add(paeth(a, b, c)),
                _ => return Err(format!("bad PNG filter {ft}")),
            };
            img[y * stride + x] = v;
        }
    }
    // sample (x, channel) of row y as 0..=255 (16-bit keeps the high byte); raw index for palettes
    let sample = |y: usize, x: usize, ch: usize| -> u8 {
        let row = y * stride;
        match depth {
            8 => img[row + x * channels + ch],
            16 => img[row + (x * channels + ch) * 2],
            d => {
                let d = d as usize;
                let bit = x * d;
                let byte = img[row + bit / 8];
                let v = (byte >> (8 - d - bit % 8)) & ((1u16 << d) - 1) as u8;
                if ctype == 3 {
                    v
                } else {
                    (v as u16 * 255 / ((1u16 << d) - 1)) as u8
                }
            }
        }
    };
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let px = match ctype {
                6 => [sample(y, x, 0), sample(y, x, 1), sample(y, x, 2), sample(y, x, 3)],
                2 => [sample(y, x, 0), sample(y, x, 1), sample(y, x, 2), 255],
                4 => {
                    let g = sample(y, x, 0);
                    [g, g, g, sample(y, x, 1)]
                }
                0 => {
                    let g = sample(y, x, 0);
                    [g, g, g, 255]
                }
                _ => {
                    let i = sample(y, x, 0) as usize;
                    let rgb = plte.get(i * 3..i * 3 + 3).ok_or("PNG palette index out of range")?;
                    [rgb[0], rgb[1], rgb[2], trns.get(i).copied().unwrap_or(255)]
                }
            };
            out.extend_from_slice(&px);
        }
    }
    Ok((w, h, out))
}

/// A Minecraft skin PNG (64x32 or 64x64) as the 64x32 RGBA bytes the 0.14 client takes.
pub fn load_png(path: &Path) -> Result<Vec<u8>, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let (w, h, rgba) = decode_png(&data)?;
    if w != 64 || (h != 32 && h != 64) {
        return Err(format!("a skin must be 64x32 or 64x64 pixels, this one is {w}x{h}"));
    }
    Ok(rgba[..SKIN_BYTES].to_vec())
}

/// Skins loaded from the `skins` folder; anything missing falls back to the generated skins.
#[derive(Default)]
pub struct SkinSet {
    pub bots: Vec<Vec<u8>>,
    pub shop: Option<Vec<u8>>,
}

impl SkinSet {
    pub fn load(dir: &Path) -> SkinSet {
        let mut set = SkinSet::default();
        let bots_dir = dir.join("bots");
        if std::fs::create_dir_all(&bots_dir).is_ok() {
            let readme = dir.join("README.txt");
            if !readme.exists() {
                let _ = std::fs::write(&readme, "Put 64x32 or 64x64 Minecraft skin PNGs here:\n  skins/bots/*.png  - bots wear these (any number, they take turns)\n  skins/shop.png    - the item shop keeper\nRestart the server after changing them.\n");
            }
        }
        let mut files: Vec<_> = std::fs::read_dir(&bots_dir)
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().map_or(false, |x| x.eq_ignore_ascii_case("png"))).collect())
            .unwrap_or_default();
        files.sort();
        for f in files {
            match load_png(&f) {
                Ok(s) => {
                    log::info!("bot skin loaded: {}", f.display());
                    set.bots.push(s);
                }
                Err(e) => log::warn!("bot skin {} skipped: {e}", f.display()),
            }
        }
        let shop = dir.join("shop.png");
        if shop.exists() {
            match load_png(&shop) {
                Ok(s) => {
                    log::info!("shop skin loaded: {}", shop.display());
                    set.shop = Some(s);
                }
                Err(e) => log::warn!("shop skin {} skipped: {e}", shop.display()),
            }
        }
        if set.bots.is_empty() {
            log::info!("no PNGs in {}: bots use the generated team skins", bots_dir.display());
        }
        set
    }

    pub fn bot(&self, n: usize, team: usize) -> Vec<u8> {
        if self.bots.is_empty() {
            team_skin(team)
        } else {
            self.bots[n % self.bots.len()].clone()
        }
    }

    pub fn shop(&self) -> Vec<u8> {
        self.shop.clone().unwrap_or_else(npc_skin)
    }
}

#[cfg(test)]
mod png_tests {
    use super::*;

    /// 2x1 RGBA PNG with one stored (uncompressed) deflate block; CRCs are not checked.
    fn tiny_png() -> Vec<u8> {
        let raw = [0u8, 10, 20, 30, 255, 40, 50, 60, 128]; // filter 0 + two RGBA pixels
        let mut z = vec![0x78, 0x01, 0x01, raw.len() as u8, 0, !(raw.len() as u8), 0xff];
        z.extend_from_slice(&raw);
        z.extend_from_slice(&[0, 0, 0, 0]); // adler (ignored)
        let mut p = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut chunk = |kind: &[u8; 4], body: &[u8]| {
            p.extend_from_slice(&(body.len() as u32).to_be_bytes());
            p.extend_from_slice(kind);
            p.extend_from_slice(body);
            p.extend_from_slice(&[0, 0, 0, 0]);
        };
        chunk(b"IHDR", &[0, 0, 0, 2, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
        chunk(b"IDAT", &z);
        chunk(b"IEND", &[]);
        p
    }

    #[test]
    fn decodes_a_small_rgba_png() {
        let (w, h, px) = decode_png(&tiny_png()).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(px, vec![10, 20, 30, 255, 40, 50, 60, 128]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode_png(b"hello").is_err());
    }
}
