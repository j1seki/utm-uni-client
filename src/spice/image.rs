//! SPICE image decoding (bitmaps, JPEG, surfaces and the image cache).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::error::{Error, Result};
use crate::spice::protocol::*;
use crate::spice::surface::Canvas;

#[derive(Debug, Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub has_alpha: bool,
}

impl DecodedImage {
    pub fn sample(&self, x: i32, y: i32) -> [u8; 4] {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return [0, 0, 0, 0];
        }
        let index = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        [
            self.pixels[index],
            self.pixels[index + 1],
            self.pixels[index + 2],
            self.pixels[index + 3],
        ]
    }

    /// Nearest-neighbour scaling into a destination size.  Returns the
    /// destination pixels as RGBA.
    pub fn scaled(&self, width: u32, height: u32) -> Vec<u8> {
        if width == self.width && height == self.height {
            return self.pixels.clone();
        }
        let mut out = vec![0u8; width as usize * height as usize * 4];
        for y in 0..height {
            let sy = if height <= 1 {
                0
            } else {
                (y as u64 * self.height as u64 / height as u64) as i32
            };
            for x in 0..width {
                let sx = if width <= 1 {
                    0
                } else {
                    (x as u64 * self.width as u64 / width as u64) as i32
                };
                let p = self.sample(sx, sy);
                let index = ((y as usize) * (width as usize) + (x as usize)) * 4;
                out[index..index + 4].copy_from_slice(&p);
            }
        }
        out
    }
}

#[derive(Default)]
pub struct ImageCache {
    images: HashMap<u64, Arc<DecodedImage>>,
    palettes: HashMap<u64, Arc<Vec<u32>>>,
}

impl ImageCache {
    pub fn get(&self, id: u64) -> Option<Arc<DecodedImage>> {
        self.images.get(&id).cloned()
    }

    pub fn clear(&mut self) {
        self.images.clear();
        self.palettes.clear();
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining() {
            return Err(Error::Spice("truncated image data".into()));
        }
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }

    fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
}

/// Decode an image referenced by a 32-bit offset relative to the start of the
/// SPICE message.
pub fn decode_image(
    message: &[u8],
    offset: usize,
    cache: &mut ImageCache,
    surfaces: &HashMap<u32, Arc<RwLock<Canvas>>>,
    target_id: u32,
    target: &Canvas,
) -> Result<Option<Arc<DecodedImage>>> {
    if offset == 0 || offset >= message.len() {
        return Ok(None);
    }
    let mut cur = Cursor::new(message, offset);
    let id = cur.u64()?;
    let ty = cur.u8()?;
    let flags = cur.u8()?;
    let width = cur.u32()?;
    let height = cur.u32()?;

    if ty == IMAGE_TYPE_FROM_CACHE || ty == IMAGE_TYPE_FROM_CACHE_LOSSLESS {
        return cache
            .get(id)
            .map(Some)
            .ok_or_else(|| Error::Spice(format!("image {id} missing from cache")));
    }

    let image = match ty {
        IMAGE_TYPE_SURFACE => {
            let surface_id = cur.u32()?;
            let surface = if surface_id == target_id {
                target.clone()
            } else {
                let arc = surfaces
                    .get(&surface_id)
                    .ok_or_else(|| Error::Spice(format!("unknown surface {surface_id}")))?;
                arc.read()
                    .map_err(|_| Error::Spice("surface lock poisoned".into()))?
                    .clone()
            };
            DecodedImage {
                width: surface.width,
                height: surface.height,
                pixels: surface.pixels,
                has_alpha: true,
            }
        }
        IMAGE_TYPE_BITMAP => decode_bitmap(&mut cur, width, height, flags, cache)?,
        IMAGE_TYPE_JPEG => {
            let size = cur.u32()? as usize;
            let data = cur.take(size)?;
            decode_jpeg(data)?
        }
        IMAGE_TYPE_JPEG_ALPHA => {
            let _alpha_flags = cur.u8()?;
            let jpeg_size = cur.u32()? as usize;
            let data_size = cur.u32()? as usize;
            let data = cur.take(data_size)?;
            let jpeg = data.get(..jpeg_size).unwrap_or(data);
            let mut image = decode_jpeg(jpeg)?;
            // The remaining bytes are an alpha plane.  It is often zlib
            // compressed; if it is not, apply it directly.
            if data.len() > jpeg_size {
                let alpha = &data[jpeg_size..];
                let alpha = crate::spice::image::maybe_inflate(alpha);
                if alpha.len() >= (image.width as usize * image.height as usize) {
                    for (i, chunk) in image.pixels.chunks_exact_mut(4).enumerate() {
                        chunk[3] = alpha[i];
                    }
                    image.has_alpha = true;
                }
            }
            image
        }
        IMAGE_TYPE_ZLIB_GLZ_RGB => {
            let glz_size = cur.u32()? as usize;
            let size = cur.u32()? as usize;
            let data = cur.take(size)?;
            if glz_size != 0 {
                return Err(Error::Unsupported(
                    "GLZ-compressed images are not supported".into(),
                ));
            }
            let raw = inflate(data)?;
            rgba_from_raw(&raw, width, height, 4)?
        }
        IMAGE_TYPE_LZ_RGB | IMAGE_TYPE_GLZ_RGB => {
            return Err(Error::Unsupported(
                "LZ/GLZ images are not supported (server ignored OFF preference)".into(),
            ))
        }
        IMAGE_TYPE_LZ_PLT => {
            return Err(Error::Unsupported(
                "LZ palette images are not supported".into(),
            ))
        }
        IMAGE_TYPE_LZ4 => return Err(Error::Unsupported("LZ4 images are not supported".into())),
        IMAGE_TYPE_QUIC => return Err(Error::Unsupported("QUIC images are not supported".into())),
        other => return Err(Error::Spice(format!("unsupported image type {other}"))),
    };

    let image = Arc::new(image);
    if flags & IMAGE_FLAG_CACHE_ME != 0 && id != 0 {
        cache.images.insert(id, Arc::clone(&image));
    }
    Ok(Some(image))
}

fn decode_bitmap(
    cur: &mut Cursor<'_>,
    width: u32,
    height: u32,
    _image_flags: u8,
    cache: &mut ImageCache,
) -> Result<DecodedImage> {
    let format = cur.u8()?;
    let flags = cur.u8()?;
    let _x = cur.u32()?;
    let _y = cur.u32()?;
    let stride = cur.i32()?;

    let palette = if flags & BITMAP_FLAG_PAL_FROM_CACHE != 0 {
        let id = cur.u64()?;
        cache.palettes.get(&id).cloned()
    } else {
        let offset = cur.u32()? as usize;
        if offset != 0 {
            let mut pcur = Cursor::new(cur.data, offset);
            let unique = pcur.u64()?;
            let count = pcur.u16()? as usize;
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                entries.push(pcur.u32()?);
            }
            let entries = Arc::new(entries);
            if flags & BITMAP_FLAG_PAL_CACHE_ME != 0 {
                cache.palettes.insert(unique, Arc::clone(&entries));
            }
            Some(entries)
        } else {
            None
        }
    };

    let row_bytes = stride.unsigned_abs() as usize;
    if row_bytes == 0 {
        return Err(Error::Spice("bitmap has zero stride".into()));
    }
    let total = row_bytes
        .checked_mul(height as usize)
        .ok_or_else(|| Error::Spice("bitmap is too large".into()))?;
    let data = cur.take(total)?;

    let top_down = flags & BITMAP_FLAG_TOP_DOWN != 0;
    let flip = top_down == (stride < 0);
    let mut out = vec![0u8; width as usize * height as usize * 4];

    for y in 0..height as usize {
        let src_y = if flip { height as usize - 1 - y } else { y };
        let row = &data[src_y * row_bytes..(src_y + 1) * row_bytes];
        for x in 0..width as usize {
            let rgba = match format {
                BITMAP_FMT_32BIT => {
                    let i = x * 4;
                    if i + 3 >= row.len() {
                        [0, 0, 0, 255]
                    } else {
                        [row[i + 2], row[i + 1], row[i], 255]
                    }
                }
                BITMAP_FMT_RGBA => {
                    let i = x * 4;
                    if i + 3 >= row.len() {
                        [0, 0, 0, 0]
                    } else {
                        [row[i + 2], row[i + 1], row[i], row[i + 3]]
                    }
                }
                BITMAP_FMT_24BIT => {
                    let i = x * 3;
                    if i + 2 >= row.len() {
                        [0, 0, 0, 255]
                    } else {
                        [row[i + 2], row[i + 1], row[i], 255]
                    }
                }
                BITMAP_FMT_16BIT => {
                    let i = x * 2;
                    if i + 1 >= row.len() {
                        [0, 0, 0, 255]
                    } else {
                        let v = u16::from_le_bytes([row[i], row[i + 1]]) as u32;
                        let r = ((v >> 10) & 0x1f) as u8;
                        let g = ((v >> 5) & 0x1f) as u8;
                        let b = (v & 0x1f) as u8;
                        [
                            (r << 3) | (r >> 2),
                            (g << 3) | (g >> 2),
                            (b << 3) | (b >> 2),
                            255,
                        ]
                    }
                }
                BITMAP_FMT_8BIT => {
                    let index = row.get(x).copied().unwrap_or(0) as usize;
                    palette_color(palette.as_deref().map(|v| v.as_slice()), index)
                }
                BITMAP_FMT_8BIT_A => {
                    let alpha = row.get(x).copied().unwrap_or(0);
                    [0, 0, 0, alpha]
                }
                BITMAP_FMT_4BIT_LE | BITMAP_FMT_4BIT_BE => {
                    let byte = row.get(x / 2).copied().unwrap_or(0);
                    let index = if format == BITMAP_FMT_4BIT_LE {
                        if x % 2 == 0 {
                            byte & 0x0f
                        } else {
                            byte >> 4
                        }
                    } else if x % 2 == 0 {
                        byte >> 4
                    } else {
                        byte & 0x0f
                    } as usize;
                    palette_color(palette.as_deref().map(|v| v.as_slice()), index)
                }
                BITMAP_FMT_1BIT_LE | BITMAP_FMT_1BIT_BE => {
                    let byte = row.get(x / 8).copied().unwrap_or(0);
                    let bit = x % 8;
                    let set = if format == BITMAP_FMT_1BIT_LE {
                        (byte >> bit) & 1 != 0
                    } else {
                        (byte >> (7 - bit)) & 1 != 0
                    };
                    palette_color(palette.as_deref().map(|v| v.as_slice()), set as usize)
                }
                other => return Err(Error::Spice(format!("unsupported bitmap format {other}"))),
            };
            let index = (y * width as usize + x) * 4;
            out[index..index + 4].copy_from_slice(&rgba);
        }
    }

    Ok(DecodedImage {
        width,
        height,
        pixels: out,
        has_alpha: matches!(format, BITMAP_FMT_RGBA | BITMAP_FMT_8BIT_A),
    })
}

fn palette_color(palette: Option<&[u32]>, index: usize) -> [u8; 4] {
    match palette.and_then(|p| p.get(index)) {
        Some(color) => [
            ((color >> 16) & 0xff) as u8,
            ((color >> 8) & 0xff) as u8,
            (color & 0xff) as u8,
            255,
        ],
        None => [0, 0, 0, 255],
    }
}

fn decode_jpeg(data: &[u8]) -> Result<DecodedImage> {
    let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(data));
    let pixels = decoder
        .decode()
        .map_err(|e| Error::Spice(format!("JPEG decode failed: {e}")))?;
    let info = decoder
        .info()
        .ok_or_else(|| Error::Spice("JPEG has no frame info".into()))?;
    let width = info.width as u32;
    let height = info.height as u32;
    let components = info.pixel_format.pixel_bytes();
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    match components {
        1 => {
            for value in pixels {
                rgba.extend_from_slice(&[value, value, value, 255]);
            }
        }
        3 => {
            for chunk in pixels.chunks_exact(3) {
                rgba.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
        }
        4 => {
            for chunk in pixels.chunks_exact(4) {
                rgba.extend_from_slice(chunk);
            }
        }
        _ => {
            return Err(Error::Spice(format!(
                "unsupported JPEG component count {components}"
            )))
        }
    }
    Ok(DecodedImage {
        width,
        height,
        pixels: rgba,
        has_alpha: components == 4,
    })
}

fn rgba_from_raw(raw: &[u8], width: u32, height: u32, bpp: usize) -> Result<DecodedImage> {
    let expected = width as usize * height as usize * bpp;
    if raw.len() < expected {
        return Err(Error::Spice("decompressed image is truncated".into()));
    }
    let mut out = vec![0u8; width as usize * height as usize * 4];
    for i in 0..width as usize * height as usize {
        let p = &raw[i * bpp..i * bpp + bpp];
        let rgba = match bpp {
            4 => [p[2], p[1], p[0], 255],
            3 => [p[2], p[1], p[0], 255],
            _ => [0, 0, 0, 255],
        };
        out[i * 4..i * 4 + 4].copy_from_slice(&rgba);
    }
    Ok(DecodedImage {
        width,
        height,
        pixels: out,
        has_alpha: false,
    })
}

fn inflate(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut decoder = flate2::read::ZlibDecoder::new(data);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| Error::Spice(format!("zlib decode failed: {e}")))?;
    Ok(out)
}

pub(crate) fn maybe_inflate(data: &[u8]) -> Vec<u8> {
    inflate(data).unwrap_or_else(|_| data.to_vec())
}
