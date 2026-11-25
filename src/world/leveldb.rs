//! Minimal read-only LevelDB reader + MCPE 0.11-0.16 world importer.
//!
//! Reads every `*.ldb` / `*.sst` table and `*.log` file in `<world>/db/` (blocks stored raw or
//! snappy-compressed), keeps the newest value per key, and turns the legacy terrain records
//! (key = `i32le x, i32le z, 0x30`; value = 32768 block ids, 16384 meta nibbles, ...) into chunks.
//! No MANIFEST is consulted: the highest sequence number wins, deletions are honoured.

use std::collections::HashMap;
use std::io;
use std::path::Path;

use super::chunk::{Chunk, BLOCKS, META};
use super::World;

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

fn varint(b: &[u8], p: &mut usize) -> io::Result<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*p).ok_or_else(|| bad("truncated varint"))?;
        *p += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(bad("varint too long"))
}

pub fn snappy_decompress(src: &[u8]) -> io::Result<Vec<u8>> {
    let mut p = 0;
    let total = varint(src, &mut p)? as usize;
    if total > 64 << 20 {
        return Err(bad("snappy: absurd length"));
    }
    let mut out: Vec<u8> = Vec::with_capacity(total);
    while p < src.len() {
        let tag = src[p];
        p += 1;
        match tag & 3 {
            0 => {
                let mut len = (tag >> 2) as usize;
                if len >= 60 {
                    let n = len - 59;
                    let bytes = src.get(p..p + n).ok_or_else(|| bad("snappy: truncated"))?;
                    len = bytes.iter().rev().fold(0usize, |a, &b| (a << 8) | b as usize);
                    p += n;
                }
                len += 1;
                out.extend_from_slice(src.get(p..p + len).ok_or_else(|| bad("snappy: truncated literal"))?);
                p += len;
            }
            kind => {
                let (len, off) = match kind {
                    1 => {
                        let b = *src.get(p).ok_or_else(|| bad("snappy: truncated"))? as usize;
                        p += 1;
                        (4 + ((tag >> 2) & 7) as usize, (((tag >> 5) as usize) << 8) | b)
                    }
                    2 => {
                        let b = src.get(p..p + 2).ok_or_else(|| bad("snappy: truncated"))?;
                        p += 2;
                        (1 + (tag >> 2) as usize, u16::from_le_bytes([b[0], b[1]]) as usize)
                    }
                    _ => {
                        let b = src.get(p..p + 4).ok_or_else(|| bad("snappy: truncated"))?;
                        p += 4;
                        (1 + (tag >> 2) as usize, u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
                    }
                };
                if off == 0 || off > out.len() {
                    return Err(bad("snappy: bad copy offset"));
                }
                for _ in 0..len {
                    out.push(out[out.len() - off]);
                }
            }
        }
        if out.len() > total {
            return Err(bad("snappy: overrun"));
        }
    }
    if out.len() != total {
        return Err(bad("snappy: length mismatch"));
    }
    Ok(out)
}


// ---- inflate (RFC 1951), enough for LevelDB blocks ---------------------------------------------

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    bit: u32,
    n: u32,
}

impl Bits<'_> {
    fn need(&mut self, k: u32) -> io::Result<()> {
        while self.n < k {
            let b = *self.d.get(self.pos).ok_or_else(|| bad("inflate: truncated"))?;
            self.pos += 1;
            self.bit |= u32::from(b) << self.n;
            self.n += 8;
        }
        Ok(())
    }

    fn get(&mut self, k: u32) -> io::Result<u32> {
        if k == 0 {
            return Ok(0);
        }
        self.need(k)?;
        let v = self.bit & ((1u32 << k) - 1);
        self.bit >>= k;
        self.n -= k;
        Ok(v)
    }
}

struct Huff {
    count: [u16; 16],
    symbol: Vec<u16>,
}

impl Huff {
    fn new(lengths: &[u8]) -> Huff {
        let mut count = [0u16; 16];
        for &l in lengths {
            count[l as usize] += 1;
        }
        count[0] = 0;
        let mut offs = [0u16; 16];
        for i in 1..16 {
            offs[i] = offs[i - 1] + count[i - 1];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbol[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        Huff { count, symbol }
    }

    fn decode(&self, b: &mut Bits) -> io::Result<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= b.get(1)? as i32;
            let c = i32::from(self.count[len]);
            if code - c < first {
                return Ok(self.symbol[(index + (code - first)) as usize]);
            }
            index += c;
            first = (first + c) << 1;
            code <<= 1;
        }
        Err(bad("inflate: bad code"))
    }
}

const LEN_BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

fn codes(b: &mut Bits, out: &mut Vec<u8>, lit: &Huff, dist: &Huff) -> io::Result<()> {
    loop {
        let sym = lit.decode(b)? as usize;
        match sym {
            0..=255 => out.push(sym as u8),
            256 => return Ok(()),
            257..=285 => {
                let i = sym - 257;
                let len = LEN_BASE[i] as usize + b.get(u32::from(LEN_EXTRA[i]))? as usize;
                let ds = dist.decode(b)? as usize;
                if ds >= 30 {
                    return Err(bad("inflate: bad distance code"));
                }
                let d = DIST_BASE[ds] as usize + b.get(u32::from(DIST_EXTRA[ds]))? as usize;
                if d > out.len() {
                    return Err(bad("inflate: distance too far"));
                }
                for _ in 0..len {
                    out.push(out[out.len() - d]);
                }
            }
            _ => return Err(bad("inflate: bad length code")),
        }
        if out.len() > 64 << 20 {
            return Err(bad("inflate: output too large"));
        }
    }
}

/// Raw deflate stream.
pub fn inflate(src: &[u8]) -> io::Result<Vec<u8>> {
    let mut b = Bits { d: src, pos: 0, bit: 0, n: 0 };
    let mut out = Vec::new();
    loop {
        let last = b.get(1)?;
        match b.get(2)? {
            0 => {
                b.bit = 0;
                b.n = 0;
                let h = src.get(b.pos..b.pos + 4).ok_or_else(|| bad("inflate: truncated"))?;
                let len = u16::from_le_bytes([h[0], h[1]]) as usize;
                if u16::from_le_bytes([h[2], h[3]]) != !(len as u16) {
                    return Err(bad("inflate: bad stored block"));
                }
                b.pos += 4;
                out.extend_from_slice(src.get(b.pos..b.pos + len).ok_or_else(|| bad("inflate: truncated"))?);
                b.pos += len;
            }
            1 => {
                let mut l = [8u8; 288];
                l[144..256].fill(9);
                l[256..280].fill(7);
                codes(&mut b, &mut out, &Huff::new(&l), &Huff::new(&[5u8; 30]))?;
            }
            2 => {
                let (nlen, ndist, ncode) = (b.get(5)? as usize + 257, b.get(5)? as usize + 1, b.get(4)? as usize + 4);
                const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
                let mut cl = [0u8; 19];
                for &o in ORDER.iter().take(ncode) {
                    cl[o] = b.get(3)? as u8;
                }
                let ch = Huff::new(&cl);
                let mut lens = vec![0u8; nlen + ndist];
                let mut i = 0;
                while i < nlen + ndist {
                    let s = ch.decode(&mut b)?;
                    let (val, rep) = match s {
                        0..=15 => (s as u8, 1),
                        16 => {
                            if i == 0 {
                                return Err(bad("inflate: repeat with no previous length"));
                            }
                            (lens[i - 1], 3 + b.get(2)? as usize)
                        }
                        17 => (0, 3 + b.get(3)? as usize),
                        _ => (0, 11 + b.get(7)? as usize),
                    };
                    if i + rep > nlen + ndist {
                        return Err(bad("inflate: too many lengths"));
                    }
                    lens[i..i + rep].fill(val);
                    i += rep;
                }
                codes(&mut b, &mut out, &Huff::new(&lens[..nlen]), &Huff::new(&lens[nlen..]))?;
            }
            _ => return Err(bad("inflate: bad block type")),
        }
        if last == 1 {
            return Ok(out);
        }
    }
}

/// Mojang's LevelDB stores zlib streams either with the 2-byte header (type 2) or raw (type 4).
pub fn decompress_zlib_or_raw(src: &[u8]) -> io::Result<Vec<u8>> {
    let hdr = src.len() > 2 && src[0] & 0x0f == 8 && (u16::from(src[0]) * 256 + u16::from(src[1])) % 31 == 0;
    inflate(if hdr { &src[2..] } else { src })
}

type Entries = HashMap<Vec<u8>, (u64, Option<Vec<u8>>)>;

fn put(map: &mut Entries, key: Vec<u8>, seq: u64, val: Option<Vec<u8>>) {
    match map.get(&key) {
        Some((s, _)) if *s >= seq => {}
        _ => {
            map.insert(key, (seq, val));
        }
    }
}

/// Entries of one block: (key, value).
fn block_entries(b: &[u8]) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    if b.len() < 4 {
        return Err(bad("block too small"));
    }
    let restarts = u32::from_le_bytes([b[b.len() - 4], b[b.len() - 3], b[b.len() - 2], b[b.len() - 1]]) as usize;
    let end = b.len().checked_sub(4 + restarts * 4).ok_or_else(|| bad("bad restart array"))?;
    let (mut p, mut key, mut out) = (0usize, Vec::<u8>::new(), Vec::new());
    while p < end {
        let shared = varint(b, &mut p)? as usize;
        let non_shared = varint(b, &mut p)? as usize;
        let vlen = varint(b, &mut p)? as usize;
        if shared > key.len() || p + non_shared + vlen > end {
            return Err(bad("bad block entry"));
        }
        key.truncate(shared);
        key.extend_from_slice(&b[p..p + non_shared]);
        p += non_shared;
        out.push((key.clone(), b[p..p + vlen].to_vec()));
        p += vlen;
    }
    Ok(out)
}

fn read_block(file: &[u8], off: usize, size: usize) -> io::Result<Vec<u8>> {
    let raw = file.get(off..off + size + 5).ok_or_else(|| bad("block outside file"))?;
    match raw[size] {
        0 => Ok(raw[..size].to_vec()),
        1 => snappy_decompress(&raw[..size]),
        2 | 4 => decompress_zlib_or_raw(&raw[..size]),
        t => Err(bad(&format!("unsupported block compression {t}"))),
    }
}

pub fn read_table(file: &[u8], map: &mut Entries) -> io::Result<()> {
    if file.len() < 48 {
        return Err(bad("table too small"));
    }
    let f = &file[file.len() - 48..];
    let mut p = 0;
    let _meta = (varint(f, &mut p)?, varint(f, &mut p)?);
    let (ioff, isize) = (varint(f, &mut p)? as usize, varint(f, &mut p)? as usize);
    let index = block_entries(&read_block(file, ioff, isize)?)?;
    for (_, handle) in index {
        let mut q = 0;
        let (off, size) = (varint(&handle, &mut q)? as usize, varint(&handle, &mut q)? as usize);
        for (ik, v) in block_entries(&read_block(file, off, size)?)? {
            if ik.len() < 8 {
                continue;
            }
            let (user, tail) = ik.split_at(ik.len() - 8);
            let tag = u64::from_le_bytes(tail.try_into().unwrap());
            let (seq, kind) = (tag >> 8, tag & 0xff);
            put(map, user.to_vec(), seq, (kind == 1).then_some(v));
        }
    }
    Ok(())
}

pub fn read_log(file: &[u8], map: &mut Entries) -> io::Result<()> {
    const BLOCK: usize = 32768;
    let (mut batch, mut pos) = (Vec::<u8>::new(), 0usize);
    let mut batches: Vec<Vec<u8>> = Vec::new();
    while pos + 7 <= file.len() {
        let in_block = pos % BLOCK;
        if BLOCK - in_block < 7 {
            pos += BLOCK - in_block;
            continue;
        }
        let len = u16::from_le_bytes([file[pos + 4], file[pos + 5]]) as usize;
        let kind = file[pos + 6];
        pos += 7;
        let Some(data) = file.get(pos..pos + len) else { break }; // torn tail
        pos += len;
        match kind {
            1 => batches.push(data.to_vec()),
            2 => batch = data.to_vec(),
            3 => batch.extend_from_slice(data),
            4 => {
                batch.extend_from_slice(data);
                batches.push(std::mem::take(&mut batch));
            }
            _ => {}
        }
    }
    for b in batches {
        if b.len() < 12 {
            continue;
        }
        let mut seq = u64::from_le_bytes(b[..8].try_into().unwrap());
        let count = u32::from_le_bytes(b[8..12].try_into().unwrap());
        let mut p = 12;
        for _ in 0..count {
            let Some(&kind) = b.get(p) else { break };
            p += 1;
            let klen = varint(&b, &mut p)? as usize;
            let key = b.get(p..p + klen).ok_or_else(|| bad("log: bad key"))?.to_vec();
            p += klen;
            if kind == 1 {
                let vlen = varint(&b, &mut p)? as usize;
                let val = b.get(p..p + vlen).ok_or_else(|| bad("log: bad value"))?.to_vec();
                p += vlen;
                put(map, key, seq, Some(val));
            } else {
                put(map, key, seq, None);
            }
            seq += 1;
        }
    }
    Ok(())
}

/// Spawn point from `level.dat` (little-endian NBT; looks for the `SpawnX/Y/Z` int tags).
pub fn level_spawn(level_dat: &[u8]) -> Option<(i32, i32, i32)> {
    let find = |name: &str| -> Option<i32> {
        let pat: Vec<u8> = [&[3u8][..], &(name.len() as u16).to_le_bytes(), name.as_bytes()].concat();
        let at = level_dat.windows(pat.len()).position(|w| w == &pat[..])? + pat.len();
        Some(i32::from_le_bytes(level_dat.get(at..at + 4)?.try_into().ok()?))
    };
    Some((find("SpawnX")?, find("SpawnY")?, find("SpawnZ")?))
}

/// Terrain chunks from both layouts: the 0.9-0.17 whole-column record (tag 0x30) and the 1.0+
/// sub-chunk records (tag 0x2f, version 0 = plain 4096 ids + 2048 meta nibbles). Paletted
/// sub-chunks (version 1/8, MCPE 1.2.13+) are not supported.
pub fn chunks_from(map: &Entries) -> Vec<Chunk> {
    let mut cols: HashMap<(i32, i32), (Vec<u8>, Vec<u8>)> = HashMap::new();
    for (k, (_, v)) in map {
        let Some(v) = v else { continue };
        if k.len() < 9 {
            continue;
        }
        let x = i32::from_le_bytes(k[0..4].try_into().unwrap());
        let z = i32::from_le_bytes(k[4..8].try_into().unwrap());
        match (k.len(), k[8]) {
            (9, 0x30) if v.len() >= BLOCKS + META => {
                cols.insert((x, z), (v[..BLOCKS].to_vec(), v[BLOCKS..BLOCKS + META].to_vec()));
            }
            (10, 0x2f) if k[9] < 8 && v.len() >= 1 + 4096 + 2048 && v[0] == 0 => {
                let e = cols.entry((x, z)).or_insert_with(|| (vec![0; BLOCKS], vec![0; META]));
                let base = k[9] as usize * 16;
                for cx in 0..16usize {
                    for cz in 0..16usize {
                        for ly in 0..16usize {
                            let si = (cx << 8) | (cz << 4) | ly;
                            let ci = (cx << 11) | (cz << 7) | (base + ly);
                            e.0[ci] = v[1 + si];
                            let nib = (v[1 + 4096 + (si >> 1)] >> ((si & 1) * 4)) & 0x0f;
                            e.1[ci >> 1] |= nib << ((ci & 1) * 4);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let mut out: Vec<Chunk> = cols.into_iter().filter_map(|((x, z), (b, m))| Chunk::from_raw(x, z, &b, &m)).collect();
    out.sort_by_key(|c| (c.x, c.z));
    out
}

/// What kinds of records a database holds (for error messages).
fn describe(map: &Entries) -> String {
    let mut tags: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for (k, (_, v)) in map {
        if v.is_none() {
            continue;
        }
        let name = match (k.len(), k.get(8)) {
            (9, Some(t)) | (10, Some(t)) if k.len() >= 9 => format!("chunk tag 0x{t:02x}"),
            _ => format!("{}-byte key", k.len()),
        };
        *tags.entry(name).or_default() += 1;
    }
    tags.iter().map(|(k, n)| format!("{n}x {k}")).collect::<Vec<_>>().join(", ")
}

/// Imports the MCPE world folder `dir` (containing `db/` and `level.dat`).
pub fn import(dir: &Path, order_byte: bool, pad: i32) -> io::Result<World> {
    let db = dir.join("db");
    let mut files: Vec<_> = std::fs::read_dir(&db)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", db.display())))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    files.sort();
    let mut map = Entries::new();
    for f in &files {
        match f.extension().and_then(|e| e.to_str()) {
            Some("ldb") | Some("sst") => read_table(&std::fs::read(f)?, &mut map)
                .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", f.display())))?,
            Some("log") => read_log(&std::fs::read(f)?, &mut map)
                .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", f.display())))?,
            _ => {}
        }
    }
    let chunks = chunks_from(&map);
    if chunks.is_empty() {
        return Err(bad(&format!(
            "no readable terrain chunks (supported: MCPE 0.9-0.17 columns and 1.0-1.2 plain sub-chunks). Found: {}",
            describe(&map)
        )));
    }
    let spawn = std::fs::read(dir.join("level.dat")).ok().and_then(|d| level_spawn(&d)).unwrap_or((0, 64, 0));
    Ok(World::from_chunks(chunks, spawn, order_byte, pad))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inflate_matches_zlib() {
        // fixed
        assert_eq!(inflate(&[203,72,205,201,201,87,200,192,32,203,243,139,114,82,116,20,146,82,83,202,19,139,138,209,105,197,140,81,109,163,218,70,181,141,106,67,214,6,0]).unwrap(), b"hello hello hello hello world, bedwars bedwars bedwars!".repeat(20));
        // dyn
        let plain = { let mut v: Vec<u8> = [68,32,130,60,253,230,241,194,107,48,249,14,199,221,1,228,136,117,52,162,15,11,13,4,195,110,216,14,113,224,253,119,176,118,112,235,148,11,213,51,95,151,61,170,216,97,155,145,255,201,17,245,124,206,212,88,187,191,44,224,55,83,201,189,250,15,240,22,157,201,87,86,116,6,102,118,207,176,180,235,137,2,196,66,105,218,28,246,186,102,211,248,182,212,177,0,169,234,14,117,90,92,46,130,16,36,42,8,231,7,143,127,137,56,94,176,148,35,85,81,130,86,139,150,232,164,254,242,58,12,159,197,175,215,96,132,55,129,107,221,10,115,9,203,74,18,82,228,218,112,230,114,15,202,164,218,30,152,64,108,24,156,36,39,158,152,81,213,129,66,4,19,111,235,87,19,193,102,177,50,105,221,99,252,53,199,151,255,8,166,205,144,9,80,102,167,69,173,219,109,136,49,194,176,248,120,33,20,43,68,86,85,109,137,170,130,188,173,174,58,149,120,250,69,53,164,20,208,37,194,75,64,174,58,193,39,114,41,136,186,151,58,234,141,55,23,151,6,7,46,211,58,20,96,122,215,82,59,230,85,123,81,52,222,193,150,129,244,161,51,106,162,20,13,5,151,163,230,200,160,204,32,32,162,233,57,128,110,240,182,132,93,106,157,101,126,184,41,143,45].to_vec(); v.extend(std::iter::repeat(0u8).take(5000)); for _ in 0..500 { v.extend_from_slice(b"abc"); } v };
        assert_eq!(inflate(&[115,81,104,178,249,251,236,227,161,108,131,159,124,199,239,50,62,233,40,53,89,196,207,205,203,114,56,239,6,95,225,131,191,229,27,202,10,94,79,225,190,106,28,63,221,118,213,141,196,217,19,255,159,20,252,90,115,238,74,196,238,253,58,15,204,131,79,238,253,197,255,65,108,238,201,240,176,18,182,180,178,243,27,182,188,238,100,58,226,148,121,75,230,219,174,180,203,63,182,93,217,200,176,242,21,95,105,84,140,94,147,128,138,22,199,115,246,254,250,78,139,184,13,83,148,67,3,155,194,186,167,189,88,242,239,147,21,207,252,163,235,175,39,180,152,55,102,223,229,42,230,60,237,37,20,244,228,86,193,179,34,254,83,75,110,201,205,112,200,145,152,163,162,62,111,70,224,213,70,39,22,225,252,215,225,194,7,211,54,26,101,222,77,254,99,122,124,250,127,142,101,103,39,112,6,164,45,119,93,123,59,183,195,240,208,134,31,21,138,34,218,46,97,161,185,157,171,154,246,172,93,103,53,181,226,151,171,233,18,145,11,170,135,188,29,214,89,29,84,47,210,236,216,53,221,234,85,175,185,248,116,54,118,189,203,86,34,9,85,215,131,172,159,133,86,7,154,220,59,56,173,241,203,66,227,172,69,34,188,172,211,23,63,59,177,224,140,130,194,162,151,150,13,121,31,182,181,196,102,205,77,173,219,161,217,175,203,48,10,70,193,40,24,5,163,96,20,140,130,81,48,10,70,193,40,24,5,163,96,20,140,130,81,48,10,70,193,40,160,57,72,76,74,30,69,163,104,20,141,162,81,52,204,16,0]).unwrap(), plain);
        // stored
        assert_eq!(inflate(&[1,200,0,55,255,68,32,130,60,253,230,241,194,107,48,249,14,199,221,1,228,136,117,52,162,15,11,13,4,195,110,216,14,113,224,253,119,176,118,112,235,148,11,213,51,95,151,61,170,216,97,155,145,255,201,17,245,124,206,212,88,187,191,44,224,55,83,201,189,250,15,240,22,157,201,87,86,116,6,102,118,207,176,180,235,137,2,196,66,105,218,28,246,186,102,211,248,182,212,177,0,169,234,14,117,90,92,46,130,16,36,42,8,231,7,143,127,137,56,94,176,148,35,85,81,130,86,139,150,232,164,254,242,58,12,159,197,175,215,96,132,55,129,107,221,10,115,9,203,74,18,82,228,218,112,230,114,15,202,164,218,30,152,64,108,24,156,36,39,158,152,81,213,129,66,4,19,111,235,87,19,193,102,177,50,105,221,99,252,53,199,151,255,8,166,205,144,9,80,102,167,69,173,219,109]).unwrap(), &[68,32,130,60,253,230,241,194,107,48,249,14,199,221,1,228,136,117,52,162,15,11,13,4,195,110,216,14,113,224,253,119,176,118,112,235,148,11,213,51,95,151,61,170,216,97,155,145,255,201,17,245,124,206,212,88,187,191,44,224,55,83,201,189,250,15,240,22,157,201,87,86,116,6,102,118,207,176,180,235,137,2,196,66,105,218,28,246,186,102,211,248,182,212,177,0,169,234,14,117,90,92,46,130,16,36,42,8,231,7,143,127,137,56,94,176,148,35,85,81,130,86,139,150,232,164,254,242,58,12,159,197,175,215,96,132,55,129,107,221,10,115,9,203,74,18,82,228,218,112,230,114,15,202,164,218,30,152,64,108,24,156,36,39,158,152,81,213,129,66,4,19,111,235,87,19,193,102,177,50,105,221,99,252,53,199,151,255,8,166,205,144,9,80,102,167,69,173,219,109]);
        // zl
        assert_eq!(decompress_zlib_or_raw(&[120,156,203,72,205,201,201,87,200,192,32,203,243,139,114,82,116,20,146,82,83,202,19,139,138,209,105,69,76,13,163,218,70,181,141,106,27,209,218,0,37,42,151,84]).unwrap(), b"hello hello hello hello world, bedwars bedwars bedwars!".repeat(20));
        assert!(inflate(&[0xff, 0xff, 0xff]).is_err());
    }

    #[test]
    fn snappy_literals_and_copies() {
        // "abcabcabc": literal "abc" then copy len 6 offset 3 (tag type 2)
        let enc = [9u8, 2 << 2, b'a', b'b', b'c', (5 << 2) | 2, 3, 0];
        assert_eq!(snappy_decompress(&enc).unwrap(), b"abcabcabc");
        assert!(snappy_decompress(&[5, 0, b'a']).is_err());
    }

    fn rec(kind: u8, data: &[u8]) -> Vec<u8> {
        let mut v = vec![0, 0, 0, 0];
        v.extend_from_slice(&(data.len() as u16).to_le_bytes());
        v.push(kind);
        v.extend_from_slice(data);
        v
    }

    fn batch(seq: u64, ops: &[(u8, &[u8], &[u8])]) -> Vec<u8> {
        let mut b = seq.to_le_bytes().to_vec();
        b.extend_from_slice(&(ops.len() as u32).to_le_bytes());
        for (kind, k, v) in ops {
            b.push(*kind);
            b.push(k.len() as u8);
            b.extend_from_slice(k);
            if *kind == 1 {
                let mut n = v.len();
                while n >= 0x80 {
                    b.push((n & 0x7f) as u8 | 0x80);
                    n >>= 7;
                }
                b.push(n as u8);
                b.extend_from_slice(v);
            }
        }
        b
    }

    #[test]
    fn log_batches_newest_wins_and_deletes() {
        let mut key = Vec::new();
        key.extend_from_slice(&3i32.to_le_bytes());
        key.extend_from_slice(&(-2i32).to_le_bytes());
        key.push(0x30);
        let mut val = vec![0u8; BLOCKS + META + 100];
        val[5] = 42; // x=0,z=0,y=5
        val[BLOCKS] = 0x07; // meta nibble of block 0
        let other = vec![9u8; 4];
        let mut file = rec(1, &batch(10, &[(1, &key, &other)]));
        file.extend(rec(1, &batch(20, &[(1, &key, &val)])));
        let mut map = Entries::new();
        read_log(&file, &mut map).unwrap();
        let chunks = chunks_from(&map);
        assert_eq!(chunks.len(), 1);
        assert_eq!((chunks[0].x, chunks[0].z), (3, -2));
        assert_eq!(chunks[0].get(0, 5, 0).0, 42);
        assert_eq!(chunks[0].get(0, 0, 0), (0, 7));
        // a later delete removes it
        file.extend(rec(1, &batch(30, &[(0, &key, b"")])));
        let mut map = Entries::new();
        read_log(&file, &mut map).unwrap();
        assert!(chunks_from(&map).is_empty());
    }

    #[test]
    fn subchunks_v0_assemble_a_column() {
        let mut key = Vec::new();
        key.extend_from_slice(&1i32.to_le_bytes());
        key.extend_from_slice(&1i32.to_le_bytes());
        key.push(0x2f);
        key.push(2); // sub-chunk y = 2 -> world y 32..47
        let mut val = vec![0u8; 1 + 4096 + 2048];
        let si = (3 << 8) | (4 << 4) | 5; // x=3 z=4 ly=5
        val[1 + si] = 35;
        val[1 + 4096 + (si >> 1)] = 0x0e << ((si & 1) * 4); // meta 14
        let mut map = Entries::new();
        put(&mut map, key, 1, Some(val));
        let c = chunks_from(&map);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].get(3, 37, 4), (35, 14));
        assert_eq!(c[0].get(0, 0, 0), (0, 0));
    }

    #[test]
    fn spawn_from_level_dat() {
        let mut d = vec![8, 0, 0, 0, 0, 0, 0, 0];
        for (n, v) in [("SpawnX", 12i32), ("SpawnY", 70), ("SpawnZ", -5)] {
            d.push(3);
            d.extend_from_slice(&(n.len() as u16).to_le_bytes());
            d.extend_from_slice(n.as_bytes());
            d.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(level_spawn(&d), Some((12, 70, -5)));
        assert_eq!(level_spawn(b"junk"), None);
    }
}
