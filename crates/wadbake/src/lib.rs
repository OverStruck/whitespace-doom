//! DOOM IWAD -> cells: lumps converted to the one-cell-per-scalar memory
//! model, plus PLAYPAL as xterm-256 colors.

pub struct Lump {
    pub name: String,
    pub data: Vec<i64>,
}

pub struct Baked {
    pub lumps: Vec<Lump>,
    pub palettes: Vec<[i64; 256]>,
}

impl Baked {
    pub fn total_cells(&self) -> usize {
        self.lumps.iter().map(|l| l.data.len()).sum::<usize>() + self.palettes.len() * 256
    }
}

fn i16_at(b: &[u8], o: usize) -> i64 {
    i16::from_le_bytes([b[o], b[o + 1]]) as i64
}
fn u16_at(b: &[u8], o: usize) -> i64 {
    u16::from_le_bytes([b[o], b[o + 1]]) as i64
}
fn i32_at(b: &[u8], o: usize) -> Result<i64, String> {
    b.get(o..o + 4)
        .map(|s| i32::from_le_bytes(s.try_into().unwrap()) as i64)
        .ok_or_else(|| format!("read past end at {o}"))
}

const MAP_LUMPS: [&str; 10] = [
    "THINGS", "LINEDEFS", "SIDEDEFS", "VERTEXES", "SEGS", "SSECTORS", "NODES", "SECTORS", "REJECT",
    "BLOCKMAP",
];

fn is_map_marker(n: &str) -> bool {
    let b = n.as_bytes();
    (b.len() == 4 && b[0] == b'E' && b[2] == b'M' && b[1].is_ascii_digit() && b[3].is_ascii_digit())
        || (b.len() == 5 && n.starts_with("MAP") && b[3].is_ascii_digit() && b[4].is_ascii_digit())
}

fn is_named_patch(n: &str) -> bool {
    ["ST", "M_", "WI", "AMMNUM", "BRDR_"]
        .iter()
        .any(|p| n.starts_with(p))
        || matches!(
            n,
            "TITLEPIC"
                | "HELP"
                | "HELP1"
                | "HELP2"
                | "CREDIT"
                | "VICTORY2"
                | "PFUB1"
                | "PFUB2"
                | "BOSSBACK"
                | "INTERPIC"
        )
        || (n.len() == 4 && n.starts_with("END") && (b'0'..=b'6').contains(&n.as_bytes()[3]))
}

fn patch(n: &str, b: &[u8]) -> Result<Vec<i64>, String> {
    if b.len() < 8 {
        return Err(format!("{n}: patch too short"));
    }
    let w = i16_at(b, 0);
    if w <= 0 || 8 + 4 * w as usize > b.len() {
        return Err(format!("{n}: bad patch width {w}"));
    }
    let w = w as usize;
    let mut out: Vec<i64> = (0..4).map(|i| i16_at(b, 2 * i)).collect();
    for c in 0..w {
        let o = i32_at(b, 8 + 4 * c)?;
        if o < (8 + 4 * w) as i64 || o >= b.len() as i64 {
            return Err(format!("{n}: column {c} offset {o} out of range"));
        }
        out.push(o - 4 - 3 * w as i64);
    }
    out.extend(b[8 + 4 * w..].iter().map(|&x| x as i64));
    Ok(out)
}

fn texture(n: &str, b: &[u8]) -> Result<Vec<i64>, String> {
    let count = i32_at(b, 0)?;
    if count < 0 {
        return Err(format!("{n}: negative count"));
    }
    let count = count as usize;
    let mut body = Vec::new();
    let mut offs = Vec::with_capacity(count);
    for t in 0..count {
        let o = i32_at(b, 4 + 4 * t)? as usize;
        if o + 22 > b.len() {
            return Err(format!("{n}: texture {t} out of range"));
        }
        offs.push((1 + count + body.len()) as i64);
        body.extend(b[o..o + 8].iter().map(|&x| x as i64));
        body.push(i32_at(b, o + 8)?);
        body.push(i16_at(b, o + 12));
        body.push(i16_at(b, o + 14));
        body.push(i32_at(b, o + 16)?);
        let pc = i16_at(b, o + 20);
        if pc < 0 || o + 22 + 10 * pc as usize > b.len() {
            return Err(format!("{n}: texture {t} bad patchcount"));
        }
        body.push(pc);
        for k in 0..5 * pc as usize {
            body.push(i16_at(b, o + 22 + 2 * k));
        }
    }
    let mut out = vec![count as i64];
    out.extend(offs);
    out.extend(body);
    Ok(out)
}

fn map_lump(n: &str, b: &[u8]) -> Result<Vec<i64>, String> {
    // (record bytes, per-record converter)
    let rec = match n {
        "THINGS" => 10,
        "LINEDEFS" => 14,
        "SIDEDEFS" => 30,
        "VERTEXES" => 4,
        "SEGS" => 12,
        "SSECTORS" => 4,
        "NODES" => 28,
        "SECTORS" => 26,
        "REJECT" => return Ok(b.iter().map(|&x| x as i64).collect()),
        _ => 2, // BLOCKMAP
    };
    if !b.len().is_multiple_of(rec) {
        return Err(format!("{n}: size {} not a multiple of {rec}", b.len()));
    }
    let mut out = Vec::new();
    for r in b.chunks(rec) {
        match n {
            "SIDEDEFS" => {
                out.extend([i16_at(r, 0), i16_at(r, 2)]);
                out.extend(r[4..28].iter().map(|&x| x as i64));
                out.push(i16_at(r, 28));
            }
            "SECTORS" => {
                out.extend([i16_at(r, 0), i16_at(r, 2)]);
                out.extend(r[4..20].iter().map(|&x| x as i64));
                out.extend([i16_at(r, 20), i16_at(r, 22), i16_at(r, 24)]);
            }
            "NODES" => {
                out.extend((0..12).map(|i| i16_at(r, 2 * i)));
                out.extend([u16_at(r, 24), u16_at(r, 26)]);
            }
            _ => out.extend((0..rec / 2).map(|i| i16_at(r, 2 * i))),
        }
    }
    Ok(out)
}

fn xterm(r: i64, g: i64, b: i64) -> i64 {
    const LV: [i64; 6] = [0, 95, 135, 175, 215, 255];
    let rgb = |i: i64| -> (i64, i64, i64) {
        if i >= 232 {
            let v = 8 + 10 * (i - 232);
            (v, v, v)
        } else {
            let c = i - 16;
            (
                LV[(c / 36) as usize],
                LV[(c / 6 % 6) as usize],
                LV[(c % 6) as usize],
            )
        }
    };
    (16..=255)
        .min_by_key(|&i| {
            let (x, y, z) = rgb(i);
            (x - r).pow(2) + (y - g).pow(2) + (z - b).pow(2)
        })
        .unwrap()
}

pub fn bake(wad: &[u8]) -> Result<Baked, String> {
    if wad.len() < 12 || !(&wad[..4] == b"IWAD" || &wad[..4] == b"PWAD") {
        return Err("not a WAD".into());
    }
    let num = i32_at(wad, 4)?;
    let dir = i32_at(wad, 8)?;
    if num < 0 || dir < 0 || dir as usize + 16 * num as usize > wad.len() {
        return Err("bad directory".into());
    }
    let (mut lumps, mut palettes) = (Vec::new(), Vec::new());
    let mut in_patch_ns = false;
    let mut in_map = false;
    let mut in_flat_ns = false;
    for i in 0..num as usize {
        let e = dir as usize + 16 * i;
        let pos = i32_at(wad, e)?;
        let size = i32_at(wad, e + 4)?;
        let name: String = wad[e + 8..e + 16]
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| (c as char).to_ascii_uppercase())
            .collect();
        if pos < 0 || size < 0 || (pos + size) as usize > wad.len() {
            return Err(format!("{name}: lump out of range"));
        }
        let b = &wad[pos as usize..(pos + size) as usize];
        let n = name.as_str();
        let is_marker = n.ends_with("_START") || n.ends_with("_END");
        if is_marker {
            let p = n.split('_').next().unwrap();
            if matches!(p, "S" | "SS" | "P" | "P1" | "P2" | "P3") {
                in_patch_ns = n.ends_with("_START");
            }
            if matches!(p, "F" | "FF" | "F1" | "F2" | "F3") {
                in_flat_ns = n.ends_with("_START");
            }
        }
        if is_map_marker(n) {
            in_map = true;
        } else if !MAP_LUMPS.contains(&n) {
            in_map = false;
        }
        let data = if n.starts_with("DS")
            || n.starts_with("DP")
            || n.starts_with("D_")
            || n == "GENMIDI"
            || n == "DMXGUS"
        {
            vec![]
        } else if in_map && MAP_LUMPS.contains(&n) {
            map_lump(n, b)?
        } else if n == "TEXTURE1" || n == "TEXTURE2" {
            texture(n, b)?
        // PNAMES stays raw: r_data.c reads its count through `*(int *)names`
        } else if !is_marker && (in_patch_ns || (!in_flat_ns && is_named_patch(n))) {
            patch(n, b)?
        } else if n.starts_with("DEMO") && b.first() == Some(&109) {
            // v1.9 demos: linuxdoom (VERSION 110) refuses them although the
            // game logic is the same, so the attract loop showed no demos
            std::iter::once(110)
                .chain(b[1..].iter().map(|&x| x as i64))
                .collect()
        } else {
            b.iter().map(|&x| x as i64).collect()
        };
        if n == "PLAYPAL" {
            for p in b.as_chunks::<768>().0.iter().take(14) {
                let mut pal = [0i64; 256];
                for (k, c) in p.chunks(3).enumerate() {
                    pal[k] = xterm(c[0] as i64, c[1] as i64, c[2] as i64);
                }
                palettes.push(pal);
            }
        }
        lumps.push(Lump { name, data });
    }
    Ok(Baked { lumps, palettes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doom1() {
        let Ok(wad) = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../wad/doom1.wad"))
        else {
            eprintln!("skipped: no wad/doom1.wad");
            return;
        };
        let bk = bake(&wad).unwrap();
        let num = i32_at(&wad, 4).unwrap() as usize;
        let dir = i32_at(&wad, 8).unwrap() as usize;
        assert_eq!(bk.lumps.len(), num);
        for (i, l) in bk.lumps.iter().enumerate() {
            let e = dir + 16 * i;
            let raw: String = wad[e + 8..e + 16]
                .iter()
                .take_while(|&&c| c != 0)
                .map(|&c| c as char)
                .collect();
            assert_eq!(l.name, raw.to_ascii_uppercase());
        }
        assert_eq!(bk.lumps[0].name, "PLAYPAL");
        let find = |n: &str| bk.lumps.iter().find(|l| l.name == n).unwrap();
        for n in ["DSPISTOL", "DPPISTOL", "D_E1M1", "GENMIDI", "DMXGUS"] {
            assert!(find(n).data.is_empty(), "{n}");
        }
        // patch walk
        let mut patches = 0;
        let mut in_ns = false;
        let mut fl = false;
        for l in &bk.lumps {
            let n = l.name.as_str();
            if n == "S_START" || n == "P_START" {
                in_ns = true;
                continue;
            }
            if n == "S_END" || n == "P_END" {
                in_ns = false;
                continue;
            }
            if n == "F_START" {
                fl = true;
            }
            if n == "F_END" {
                fl = false;
            }
            if n.ends_with("_START") || n.ends_with("_END") {
                continue;
            }
            if !(in_ns || (!fl && is_named_patch(n))) || n.starts_with("D_") {
                continue;
            }
            if n.starts_with("ST") && l.data.is_empty() {
                continue;
            }
            patches += 1;
            let d = &l.data;
            let w = d[0] as usize;
            for c in 0..w {
                let mut p = d[4 + c] as usize;
                loop {
                    assert!(p < d.len(), "{n} col {c}");
                    if d[p] == 255 {
                        break;
                    }
                    let len = d[p + 1] as usize;
                    p += 4 + len;
                    assert!(p <= d.len(), "{n} col {c}");
                }
            }
        }
        assert!(patches > 500, "{patches}");
        let pnb = &find("PNAMES").data;
        let pn = pnb[0] + 256 * pnb[1];
        let t = &find("TEXTURE1").data;
        for k in 0..t[0] as usize {
            let o = t[1 + k] as usize;
            let pc = t[o + 12] as usize;
            for j in 0..pc {
                let pi = t[o + 13 + 5 * j + 2];
                assert!(pi >= 0 && pi < pn);
            }
        }
        let rec = [
            ("THINGS", 5),
            ("LINEDEFS", 7),
            ("SIDEDEFS", 27),
            ("VERTEXES", 2),
            ("SEGS", 6),
            ("SSECTORS", 2),
            ("NODES", 14),
            ("SECTORS", 21),
        ];
        let mut maps = 0;
        for l in &bk.lumps {
            if let Some(&(_, r)) = rec.iter().find(|x| x.0 == l.name) {
                maps += 1;
                assert_eq!(l.data.len() % r, 0, "{}", l.name);
            }
        }
        assert_eq!(maps, 9 * 8);
        assert_eq!(bk.palettes.len(), 14);
        assert!(bk
            .palettes
            .iter()
            .flatten()
            .all(|&c| (16..=255).contains(&c)));
        println!("TOTAL_CELLS {}", bk.total_cells());
    }
}
