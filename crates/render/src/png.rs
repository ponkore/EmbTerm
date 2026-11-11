//! デバッグ用の画面の保存。圧縮しない PNG（deflate の「格納」ブロック）を書く。
//! 外部のクレートも WIC（COM の初期化）も要らないように、ここで書いている。

use std::io::Write;
use std::path::Path;

fn crc32(data: &[u8], mut crc: u32) -> u32 {
    crc = !crc;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(data, crc32(kind, 0));
    out.extend_from_slice(&crc.to_be_bytes());
}

/// BGRA（`pitch` バイトごとに 1 行）の画素を RGB の PNG にする
pub fn encode_bgra(width: u32, height: u32, bgra: &[u8], pitch: usize) -> Vec<u8> {
    // 生の画像データ: 行ごとにフィルタ 0 + RGB
    let mut raw = Vec::with_capacity((width as usize * 3 + 1) * height as usize);
    for y in 0..height as usize {
        raw.push(0);
        let row = &bgra[y * pitch..y * pitch + width as usize * 4];
        for px in row.as_chunks::<4>().0 {
            raw.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    // zlib（圧縮しない格納ブロックだけ）
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65535).peekable();
    if blocks.peek().is_none() {
        z.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    while let Some(b) = blocks.next() {
        z.push(if blocks.peek().is_none() { 1 } else { 0 });
        let len = b.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

pub fn write_bgra(path: &Path, width: u32, height: u32, bgra: &[u8], pitch: usize) -> std::io::Result<()> {
    let png = encode_bgra(width, height, bgra, pitch);
    let mut f = std::fs::File::create(path)?;
    f.write_all(&png)
}

#[cfg(test)]
mod tests {
    #[test]
    fn crc_of_iend() {
        // IEND の CRC はよく知られた値
        assert_eq!(super::crc32(b"IEND", 0), 0xAE42_6082);
    }
}
