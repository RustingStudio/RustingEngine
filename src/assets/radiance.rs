//! Radiance `.hdr` (RGBE) images, decoded to half-float texels for HDR
//! environment maps.

/// Decodes a Radiance file into its size and RGBA half-float texels, top
/// row first, each channel two little-endian bytes.
// ponytail: only the standard `-Y h +X w` orientation and new-style or flat
// scanlines; old-style run-length files are rejected.
pub(super) fn decode(bytes: &[u8]) -> Result<([u32; 2], Vec<u8>), String> {
    let mut lines = bytes.split(|&byte| byte == b'\n');
    let magic = lines.next().unwrap_or_default();
    if !magic.starts_with(b"#?") {
        return Err("not a Radiance file".to_owned());
    }
    let mut offset = magic.len() + 1;
    for line in lines.by_ref() {
        offset += line.len() + 1;
        if line.is_empty() {
            break;
        }
        if line.starts_with(b"FORMAT=") && line != b"FORMAT=32-bit_rle_rgbe" {
            return Err(format!(
                "unsupported {}",
                String::from_utf8_lossy(line)
            ));
        }
    }
    let resolution = lines.next().ok_or("missing resolution")?;
    offset += resolution.len() + 1;
    let words: Vec<_> = std::str::from_utf8(resolution)
        .map_err(|error| error.to_string())?
        .split_whitespace()
        .collect();
    let [height, width] = match words[..] {
        ["-Y", height, "+X", width] => [height, width].map(str::parse::<u32>),
        _ => return Err(format!("unsupported resolution {words:?}")),
    };
    let (width, height) = (
        width.map_err(|error| error.to_string())?,
        height.map_err(|error| error.to_string())?,
    );
    let mut data = bytes.get(offset..).unwrap_or_default();
    let mut texels = Vec::with_capacity(width as usize * height as usize * 8);
    let mut scanline = vec![[0_u8; 4]; width as usize];
    for _ in 0..height {
        data = read_scanline(data, &mut scanline)?;
        for &[r, g, b, e] in &scanline {
            let scale = if e == 0 {
                0.0
            } else {
                2.0_f32.powi(i32::from(e) - 136)
            };
            for channel in [r, g, b]
                .map(|c| f32::from(c) * scale)
                .into_iter()
                .chain([1.0])
            {
                texels.extend_from_slice(&f16_bits(channel).to_le_bytes());
            }
        }
    }
    Ok(([width, height], texels))
}

/// Reads one scanline of RGBE texels and returns the bytes after it.
fn read_scanline<'a>(
    data: &'a [u8],
    scanline: &mut [[u8; 4]],
) -> Result<&'a [u8], String> {
    let truncated = || "truncated pixel data".to_owned();
    let width = scanline.len();
    let run_length = (8..0x8000).contains(&width)
        && data.len() >= 4
        && data[..2] == [2, 2]
        && usize::from(data[2]) << 8 | usize::from(data[3]) == width;
    if !run_length {
        let flat = data.get(..width * 4).ok_or_else(truncated)?;
        for (texel, bytes) in scanline.iter_mut().zip(flat.chunks_exact(4)) {
            texel.copy_from_slice(bytes);
        }
        return Ok(&data[width * 4..]);
    }
    // Each channel in turn: a count over 128 repeats the next byte, else
    // that many literal bytes follow.
    let mut at = 4;
    for channel in 0..4 {
        let mut x = 0;
        while x < width {
            let count = usize::from(*data.get(at).ok_or_else(truncated)?);
            at += 1;
            let (run, literal) = if count > 128 {
                (count - 128, false)
            } else {
                (count, true)
            };
            if run == 0 || x + run > width {
                return Err("bad run length".to_owned());
            }
            for texel in &mut scanline[x..x + run] {
                texel[channel] = *data.get(at).ok_or_else(truncated)?;
                at += usize::from(literal);
            }
            at += usize::from(!literal);
            x += run;
        }
    }
    Ok(&data[at..])
}

/// Bits of the half float nearest `value`, clamped to 0..=65504.
fn f16_bits(value: f32) -> u16 {
    if value.is_nan() || value <= 0.0 {
        return 0;
    }
    let value = value.min(65504.0);
    // Below the smallest normal half, count in steps of 2^-24.
    if value < 6.103_515_6e-5 {
        return (value * 16_777_216.0).round() as u16;
    }
    // Round the dropped 13 mantissa bits to nearest; a carry bumps the
    // exponent, which is the right result.
    let bits = value.to_bits() + 0x0fff + ((value.to_bits() >> 13) & 1);
    let exponent = (bits >> 23) as i32 - 127 + 15;
    ((exponent as u32) << 10 | (bits >> 13) & 0x3ff).min(0x7bff) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn half(bits: &[u8]) -> f32 {
        let bits = u16::from_le_bytes([bits[0], bits[1]]);
        let exponent = i32::from(bits >> 10);
        let mantissa = f32::from(bits & 0x3ff);
        if exponent == 0 {
            mantissa * 2.0_f32.powi(-24)
        } else {
            (1.0 + mantissa / 1024.0) * 2.0_f32.powi(exponent - 15)
        }
    }

    #[test]
    fn radiance_files_decode_flat_and_run_length_scanlines() {
        let header = |width: u32, height: u32| {
            format!(
                "#?RADIANCE\nFORMAT=32-bit_rle_rgbe\nEXPOSURE=1\n\n-Y {height} +X {width}\n"
            )
            .into_bytes()
        };
        // Flat: two texels, 1.0 red (128 * 2^-7) and 4.0 white.
        let mut flat = header(2, 1);
        flat.extend([128, 0, 0, 129, 128, 128, 128, 131]);
        let ([width, height], texels) = decode(&flat).unwrap();
        assert_eq!([width, height], [2, 1]);
        let values: Vec<f32> = texels.chunks(2).map(half).collect();
        assert_eq!(values, [1.0, 0.0, 0.0, 1.0, 4.0, 4.0, 4.0, 1.0]);

        // Run-length: 8 texels per row, two rows, all at exponent 128
        // (2^-8). Red is one run of 64, green 8 literals, blue a run of 0.
        let mut packed = header(8, 2);
        for row in 0..2_u8 {
            packed.extend([2, 2, 0, 8]);
            packed.extend([128 + 8, 64]);
            packed.extend([8, 0, 32, 0, 32, 0, 32, 0, 32 + row]);
            packed.extend([128 + 8, 0]);
            packed.extend([128 + 8, 128]);
        }
        let ([width, height], texels) = decode(&packed).unwrap();
        assert_eq!([width, height], [8, 2]);
        let texel = |x: usize, y: usize| -> Vec<f32> {
            let start = (y * 8 + x) * 8;
            texels[start..start + 8].chunks(2).map(half).collect()
        };
        assert_eq!(texel(0, 0), [0.25, 0.0, 0.0, 1.0]);
        assert_eq!(texel(1, 0), [0.25, 0.125, 0.0, 1.0]);
        assert_eq!(texel(7, 1), [0.25, 33.0 / 256.0, 0.0, 1.0]);

        assert!(decode(b"P6\n").is_err());
        let mut short = header(2, 1);
        short.extend([1, 2, 3]);
        assert!(decode(&short).is_err());
    }

    #[test]
    fn half_floats_round_and_clamp() {
        for value in [0.0, 1.0, 0.5, 3.140625, 65504.0, 1e-5] {
            let bits = f16_bits(value).to_le_bytes();
            assert!(
                (half(&bits) - value).abs() <= (value * 1e-3).max(2e-8),
                "{value}"
            );
        }
        assert_eq!(f16_bits(1e9), 0x7bff);
        assert_eq!(f16_bits(-1.0), 0);
        assert_eq!(f16_bits(f32::NAN), 0);
    }
}
