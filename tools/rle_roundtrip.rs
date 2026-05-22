// Roundtrip test for the delta+RLE codec: the build.rs encoder and the
// main.rs decoder must agree. The two functions below are kept verbatim copies
// of those in build.rs / src/main.rs - if you change the codec, update both.
//
// Run with:
//   rustc -O --edition 2021 tools/rle_roundtrip.rs -o /tmp/rle_roundtrip && /tmp/rle_roundtrip

const FRAME_BYTES: usize = 1024;

// ---- encoder: verbatim from build.rs ----
fn emit_op(out: &mut Vec<u8>, is_xor: bool, len: usize, literals: &[u8]) {
    assert!((1..=0x7FFF).contains(&len), "RLE run out of range: {len}");
    let header = len as u16 | if is_xor { 0x8000 } else { 0 };
    out.extend_from_slice(&header.to_le_bytes());
    if is_xor {
        out.extend_from_slice(literals);
    }
}
fn rle_encode(diff: &[u8]) -> Vec<u8> {
    const ZERO_GAP: usize = 4;
    let n = diff.len();
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < n {
        if diff[pos] == 0 {
            let start = pos;
            while pos < n && diff[pos] == 0 {
                pos += 1;
            }
            emit_op(&mut out, false, pos - start, &[]);
        } else {
            let start = pos;
            while pos < n {
                if diff[pos] != 0 {
                    pos += 1;
                } else {
                    let mut z = pos;
                    while z < n && diff[z] == 0 {
                        z += 1;
                    }
                    if z - pos >= ZERO_GAP {
                        break;
                    }
                    pos = z;
                }
            }
            emit_op(&mut out, true, pos - start, &diff[start..pos]);
        }
    }
    out
}

// ---- decoder: verbatim from src/main.rs ----
fn apply_delta_rle(blob: &[u8], buf: &mut [u8]) {
    let mut ip = 0;
    let mut pos = 0;
    while ip + 2 <= blob.len() && pos < buf.len() {
        let header = u16::from_le_bytes([blob[ip], blob[ip + 1]]);
        ip += 2;
        let is_xor = header & 0x8000 != 0;
        let mut len = ((header & 0x7FFF) as usize).min(buf.len() - pos);
        if is_xor {
            len = len.min(blob.len() - ip);
            for k in 0..len {
                buf[pos + k] ^= blob[ip + k];
            }
            ip += len;
        }
        pos += len;
    }
}

fn lcg(s: &mut u64) -> u64 {
    *s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
    *s >> 33
}

fn main() {
    let mut seed = 0x1234_5678u64;
    let mut tests = 0;

    // 1) single delta buffers at a range of change densities.
    for density in [0usize, 1, 3, 10, 50, 100, 256, 1024] {
        for _ in 0..50 {
            let mut diff = vec![0u8; FRAME_BYTES];
            for _ in 0..density {
                let idx = lcg(&mut seed) as usize % FRAME_BYTES;
                diff[idx] = (lcg(&mut seed) as u8) | 1;
            }
            let blob = rle_encode(&diff);
            let mut buf = vec![0u8; FRAME_BYTES];
            apply_delta_rle(&blob, &mut buf);
            assert_eq!(buf, diff, "single roundtrip failed (density {density})");
            tests += 1;
        }
    }

    // 2) worst case: every byte non-zero.
    let dense: Vec<u8> = (0..FRAME_BYTES).map(|i| ((i % 255) + 1) as u8).collect();
    let blob = rle_encode(&dense);
    let mut buf = vec![0u8; FRAME_BYTES];
    apply_delta_rle(&blob, &mut buf);
    assert_eq!(buf, dense, "dense roundtrip failed");
    tests += 1;

    // 3) full frame chains, encoded/decoded like build.rs + main.rs, with loop wraps.
    for trial in 0..20 {
        let n = 1 + lcg(&mut seed) as usize % 40;
        let mut frames: Vec<Vec<u8>> = Vec::new();
        let mut cur = vec![0u8; FRAME_BYTES];
        for _ in 0..(lcg(&mut seed) as usize % 200) {
            let idx = lcg(&mut seed) as usize % FRAME_BYTES;
            cur[idx] = lcg(&mut seed) as u8;
        }
        frames.push(cur.clone());
        for _ in 1..n {
            for _ in 0..(lcg(&mut seed) as usize % 60) {
                let idx = lcg(&mut seed) as usize % FRAME_BYTES;
                cur[idx] = lcg(&mut seed) as u8;
            }
            frames.push(cur.clone());
        }
        let mut blobs: Vec<Vec<u8>> = Vec::new();
        let mut prev = vec![0u8; FRAME_BYTES];
        for f in &frames {
            let diff: Vec<u8> = f.iter().zip(&prev).map(|(a, b)| a ^ b).collect();
            blobs.push(rle_encode(&diff));
            prev.clone_from(f);
        }
        let mut dbuf = vec![0u8; FRAME_BYTES];
        for loop_iter in 0..3 {
            for i in 0..n {
                if i == 0 {
                    dbuf.iter_mut().for_each(|b| *b = 0);
                }
                apply_delta_rle(&blobs[i], &mut dbuf);
                assert_eq!(dbuf, frames[i], "chain trial {trial} loop {loop_iter} frame {i}");
                tests += 1;
            }
        }
    }

    println!("ALL {tests} roundtrip tests passed");
}
