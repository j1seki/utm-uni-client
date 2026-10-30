//! SPICE display channel parsing and software rendering into RGBA surfaces.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use tracing::{debug, warn};

use crate::error::{Error, Result};
use crate::spice::image::{decode_image, DecodedImage, ImageCache};
use crate::spice::protocol::*;
use crate::spice::surface::{Canvas, Rect};

pub type SharedSurfaces = Arc<RwLock<HashMap<u32, Arc<RwLock<Canvas>>>>>;

#[derive(Debug, Clone)]
pub enum DisplayUpdate {
    Resize { width: u32, height: u32 },
    Dirty(Rect),
    Mark(bool),
    Unsupported(String),
}

pub struct DisplayRenderer {
    pub surfaces: SharedSurfaces,
    cache: ImageCache,
    warned_unsupported: bool,
}

impl Default for DisplayRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayRenderer {
    pub fn new() -> Self {
        Self {
            surfaces: Arc::new(RwLock::new(HashMap::new())),
            cache: ImageCache::default(),
            warned_unsupported: false,
        }
    }

    pub fn primary(&self) -> Option<Arc<RwLock<Canvas>>> {
        self.surfaces
            .read()
            .expect("surfaces lock poisoned")
            .get(&0)
            .cloned()
    }

    pub fn primary_size(&self) -> Option<(u32, u32)> {
        self.primary().map(|canvas| {
            let canvas = canvas.read().expect("canvas lock poisoned");
            (canvas.width, canvas.height)
        })
    }

    pub fn handle(&mut self, ty: u16, body: &[u8]) -> Result<Vec<DisplayUpdate>> {
        match ty {
            MSG_DISPLAY_MODE => self.handle_mode(body),
            MSG_DISPLAY_MARK => Ok(vec![DisplayUpdate::Mark(true)]),
            MSG_DISPLAY_RESET => self.handle_reset(),
            MSG_DISPLAY_COPY_BITS => self.handle_copy_bits(body),
            MSG_DISPLAY_SURFACE_CREATE => self.handle_surface_create(body),
            MSG_DISPLAY_SURFACE_DESTROY => self.handle_surface_destroy(body),
            MSG_DISPLAY_MONITORS_CONFIG => Ok(Vec::new()),
            MSG_DISPLAY_INVAL_LIST
            | MSG_DISPLAY_INVAL_ALL_PIXMAPS
            | MSG_DISPLAY_INVAL_PALETTE
            | MSG_DISPLAY_INVAL_ALL_PALETTES => Ok(Vec::new()),
            MSG_DISPLAY_STREAM_CREATE
            | MSG_DISPLAY_STREAM_DATA
            | MSG_DISPLAY_STREAM_CLIP
            | MSG_DISPLAY_STREAM_DESTROY
            | MSG_DISPLAY_STREAM_DESTROY_ALL
            | MSG_DISPLAY_STREAM_DATA_SIZED
            | MSG_DISPLAY_STREAM_ACTIVATE_REPORT => {
                Ok(self.unsupported("SPICE video streams are not supported"))
            }
            MSG_DISPLAY_DRAW_FILL => self.handle_fill(body),
            MSG_DISPLAY_DRAW_OPAQUE => self.handle_image_draw(body, DrawMode::Opaque),
            MSG_DISPLAY_DRAW_COPY => self.handle_image_draw(body, DrawMode::Copy),
            MSG_DISPLAY_DRAW_BLEND => self.handle_image_draw(body, DrawMode::Blend),
            MSG_DISPLAY_DRAW_BLACKNESS => self.handle_solid(body, [0, 0, 0, 255]),
            MSG_DISPLAY_DRAW_WHITENESS => self.handle_solid(body, [255, 255, 255, 255]),
            MSG_DISPLAY_DRAW_INVERS => self.handle_invers(body),
            MSG_DISPLAY_DRAW_ROP3 => self.handle_image_draw(body, DrawMode::Rop3),
            MSG_DISPLAY_DRAW_TRANSPARENT => self.handle_transparent(body),
            MSG_DISPLAY_DRAW_ALPHA_BLEND => self.handle_alpha_blend(body),
            MSG_DISPLAY_DRAW_COMPOSITE => {
                Ok(self.unsupported("composite draw operations are not supported"))
            }
            MSG_DISPLAY_DRAW_STROKE | MSG_DISPLAY_DRAW_TEXT => {
                Ok(self.unsupported("vector drawing is not supported"))
            }
            _ => {
                debug!("ignoring unhandled display message {ty}");
                Ok(Vec::new())
            }
        }
    }

    fn unsupported(&mut self, message: &str) -> Vec<DisplayUpdate> {
        if self.warned_unsupported {
            return Vec::new();
        }
        self.warned_unsupported = true;
        warn!("{message}");
        vec![DisplayUpdate::Unsupported(message.to_owned())]
    }

    fn handle_mode(&mut self, body: &[u8]) -> Result<Vec<DisplayUpdate>> {
        let mut cur = Cursor::new(body);
        let width = cur.u32()?;
        let height = cur.u32()?;
        let _bits = cur.u32()?;
        if width == 0 || height == 0 || width > 16384 || height > 16384 {
            return Err(Error::Spice(format!(
                "invalid display mode {width}x{height}"
            )));
        }
        let mut canvas = Canvas::new(width, height);
        canvas.clear([0, 0, 0, 255]);
        let mut surfaces = self.surfaces.write().expect("surfaces lock poisoned");
        surfaces.insert(0, Arc::new(RwLock::new(canvas)));
        drop(surfaces);
        self.cache.clear();
        debug!("SPICE display mode {width}x{height}");
        Ok(vec![
            DisplayUpdate::Resize { width, height },
            DisplayUpdate::Dirty(Rect::new(0, 0, width as i32, height as i32)),
        ])
    }

    fn handle_reset(&mut self) -> Result<Vec<DisplayUpdate>> {
        let primary = self
            .primary()
            .ok_or_else(|| Error::Spice("no primary surface".into()))?;
        let mut canvas = primary.write().expect("canvas lock poisoned");
        canvas.clear([0, 0, 0, 255]);
        let size = (canvas.width, canvas.height);
        Ok(vec![
            DisplayUpdate::Resize {
                width: size.0,
                height: size.1,
            },
            DisplayUpdate::Dirty(Rect::new(0, 0, size.0 as i32, size.1 as i32)),
        ])
    }

    fn handle_surface_create(&mut self, body: &[u8]) -> Result<Vec<DisplayUpdate>> {
        let mut cur = Cursor::new(body);
        let surface_id = cur.u32()?;
        let width = cur.u32()?;
        let height = cur.u32()?;
        let _format = cur.u32()?;
        let _flags = cur.u32()?;
        if width == 0 || height == 0 || width > 16384 || height > 16384 {
            return Err(Error::Spice(format!(
                "invalid surface {surface_id} size {width}x{height}"
            )));
        }
        let mut canvas = Canvas::new(width, height);
        if surface_id == 0 {
            canvas.clear([0, 0, 0, 255]);
        }
        self.surfaces
            .write()
            .expect("surfaces lock poisoned")
            .insert(surface_id, Arc::new(RwLock::new(canvas)));
        debug!("SPICE surface {surface_id} created {width}x{height}");
        if surface_id == 0 {
            Ok(vec![
                DisplayUpdate::Resize { width, height },
                DisplayUpdate::Dirty(Rect::new(0, 0, width as i32, height as i32)),
            ])
        } else {
            Ok(Vec::new())
        }
    }

    fn handle_surface_destroy(&mut self, body: &[u8]) -> Result<Vec<DisplayUpdate>> {
        let mut cur = Cursor::new(body);
        let surface_id = cur.u32()?;
        self.surfaces
            .write()
            .expect("surfaces lock poisoned")
            .remove(&surface_id);
        Ok(Vec::new())
    }

    fn handle_copy_bits(&mut self, body: &[u8]) -> Result<Vec<DisplayUpdate>> {
        let mut cur = Cursor::new(body);
        let base = parse_base(&mut cur)?;
        let src_x = cur.i32()?;
        let src_y = cur.i32()?;
        let target = self.target(base.surface_id)?;
        let mut canvas = target.write().expect("canvas lock poisoned");
        let rect = base.rect.clamp(canvas.width, canvas.height);
        canvas.copy_within(rect, src_x, src_y);
        if rect.is_empty() {
            Ok(Vec::new())
        } else {
            Ok(vec![DisplayUpdate::Dirty(rect)])
        }
    }

    fn handle_fill(&mut self, body: &[u8]) -> Result<Vec<DisplayUpdate>> {
        let mut cur = Cursor::new(body);
        let base = parse_base(&mut cur)?;
        let brush = parse_brush(&mut cur)?;
        let _rop = cur.u16()?;
        let mask = parse_mask(&mut cur)?;
        let mut cache = std::mem::take(&mut self.cache);
        let result = self.draw(&base, &mut cache, |target, surfaces, cache| {
            let mut dirty = Rect::new(0, 0, 0, 0);
            for rect in clip_rects(&base, target) {
                for y in rect.y..rect.y + rect.h {
                    for x in rect.x..rect.x + rect.w {
                        let color = match &brush {
                            Brush::Solid(color) => color_from_u32(*color),
                            Brush::Pattern { offset, pos } => {
                                let image = decode_image(
                                    body,
                                    *offset,
                                    cache,
                                    surfaces,
                                    base.surface_id,
                                    target,
                                )?;
                                match image {
                                    Some(image) => pattern_color(&image, x - pos.0, y - pos.1),
                                    None => [0, 0, 0, 255],
                                }
                            }
                            Brush::None => [0, 0, 0, 255],
                        };
                        let coverage = mask_coverage(
                            mask.as_ref(),
                            surfaces,
                            cache,
                            base.surface_id,
                            target,
                            body,
                            (x, y),
                        )?;
                        if coverage > 0 {
                            blend_pixel(target, x, y, color, coverage);
                        }
                    }
                }
                dirty = dirty.union(rect);
            }
            Ok(dirty)
        });
        self.cache = cache;
        result
    }

    fn handle_solid(&mut self, body: &[u8], color: [u8; 4]) -> Result<Vec<DisplayUpdate>> {
        let mut cur = Cursor::new(body);
        let base = parse_base(&mut cur)?;
        let mask = parse_mask(&mut cur)?;
        let mut cache = std::mem::take(&mut self.cache);
        let result = self.draw(&base, &mut cache, |target, surfaces, cache| {
            let mut dirty = Rect::new(0, 0, 0, 0);
            for rect in clip_rects(&base, target) {
                for y in rect.y..rect.y + rect.h {
                    for x in rect.x..rect.x + rect.w {
                        let coverage = mask_coverage(
                            mask.as_ref(),
                            surfaces,
                            cache,
                            base.surface_id,
                            target,
                            body,
                            (x, y),
                        )?;
                        if coverage > 0 {
                            blend_pixel(target, x, y, color, coverage);
                        }
                    }
                }
                dirty = dirty.union(rect);
            }
            Ok(dirty)
        });
        self.cache = cache;
        result
    }

    fn handle_invers(&mut self, body: &[u8]) -> Result<Vec<DisplayUpdate>> {
        let mut cur = Cursor::new(body);
        let base = parse_base(&mut cur)?;
        let mask = parse_mask(&mut cur)?;
        let mut cache = std::mem::take(&mut self.cache);
        let result = self.draw(&base, &mut cache, |target, surfaces, cache| {
            let mut dirty = Rect::new(0, 0, 0, 0);
            for rect in clip_rects(&base, target) {
                for y in rect.y..rect.y + rect.h {
                    for x in rect.x..rect.x + rect.w {
                        let coverage = mask_coverage(
                            mask.as_ref(),
                            surfaces,
                            cache,
                            base.surface_id,
                            target,
                            body,
                            (x, y),
                        )?;
                        if coverage == 0 {
                            continue;
                        }
                        let p = target.get(x, y);
                        let inv = [255 - p[0], 255 - p[1], 255 - p[2], p[3]];
                        blend_pixel(target, x, y, inv, coverage);
                    }
                }
                dirty = dirty.union(rect);
            }
            Ok(dirty)
        });
        self.cache = cache;
        result
    }

    fn handle_transparent(&mut self, body: &[u8]) -> Result<Vec<DisplayUpdate>> {
        let mut cur = Cursor::new(body);
        let base = parse_base(&mut cur)?;
        let offset = cur.u32()? as usize;
        let src_area = parse_rect(&mut cur)?;
        let src_color = cur.u32()?;
        let _true_color = cur.u32()?;
        self.draw_image_with_mask(
            base,
            body,
            offset,
            src_area,
            DrawMode::Transparent { src_color },
            None,
        )
    }

    fn handle_alpha_blend(&mut self, body: &[u8]) -> Result<Vec<DisplayUpdate>> {
        let mut cur = Cursor::new(body);
        let base = parse_base(&mut cur)?;
        let flags = cur.u8()?;
        let alpha = cur.u8()?;
        let offset = cur.u32()? as usize;
        let src_area = parse_rect(&mut cur)?;
        self.draw_image_with_mask(
            base,
            body,
            offset,
            src_area,
            DrawMode::AlphaBlend { flags, alpha },
            None,
        )
    }

    fn handle_image_draw(&mut self, body: &[u8], mode: DrawMode) -> Result<Vec<DisplayUpdate>> {
        let mut cur = Cursor::new(body);
        let base = parse_base(&mut cur)?;
        let offset = cur.u32()? as usize;
        let src_area = parse_rect(&mut cur)?;
        let mask = match mode {
            DrawMode::Opaque => {
                let _brush = parse_brush(&mut cur)?;
                let _rop = cur.u16()?;
                let _scale = cur.u8()?;
                parse_mask(&mut cur)?
            }
            DrawMode::Rop3 => {
                let _brush = parse_brush(&mut cur)?;
                let _rop3 = cur.u8()?;
                let _scale = cur.u8()?;
                parse_mask(&mut cur)?
            }
            DrawMode::Copy | DrawMode::Blend => {
                let _rop = cur.u16()?;
                let _scale = cur.u8()?;
                parse_mask(&mut cur)?
            }
            _ => None,
        };
        self.draw_image_with_mask(base, body, offset, src_area, mode, mask)
    }

    fn draw_image_with_mask(
        &mut self,
        base: Base,
        body: &[u8],
        offset: usize,
        src_area: Rect,
        mode: DrawMode,
        mask: Option<Mask>,
    ) -> Result<Vec<DisplayUpdate>> {
        let mut cache = std::mem::take(&mut self.cache);
        let result = self.draw(&base, &mut cache, |target, surfaces, cache| {
            let image = decode_image(body, offset, cache, surfaces, base.surface_id, target)?;
            let image = match image {
                Some(image) => image,
                None => return Ok(Rect::new(0, 0, 0, 0)),
            };
            let mut dirty = Rect::new(0, 0, 0, 0);
            for rect in clip_rects(&base, target) {
                for y in rect.y..rect.y + rect.h {
                    for x in rect.x..rect.x + rect.w {
                        let (sx, sy) = map_source(&base.rect, &src_area, &image, x, y);
                        let src = image.sample(sx, sy);
                        let coverage = mask_coverage(
                            mask.as_ref(),
                            surfaces,
                            cache,
                            base.surface_id,
                            target,
                            body,
                            (x, y),
                        )?;
                        let coverage = coverage as u32 * src[3] as u32 / 255;
                        if coverage == 0 {
                            continue;
                        }
                        match mode {
                            DrawMode::Copy | DrawMode::Opaque | DrawMode::Rop3 => {
                                blend_pixel(target, x, y, src, coverage as u8)
                            }
                            DrawMode::Blend => blend_pixel(target, x, y, src, coverage as u8),
                            DrawMode::Transparent { src_color } => {
                                let color = ((src[0] as u32) << 16)
                                    | ((src[1] as u32) << 8)
                                    | src[2] as u32;
                                if color != (src_color & 0x00ff_ffff) {
                                    blend_pixel(target, x, y, src, coverage as u8);
                                }
                            }
                            DrawMode::AlphaBlend { flags, alpha } => {
                                let alpha = if flags & 1 != 0 {
                                    (alpha as u32 * src[3] as u32 / 255) as u8
                                } else {
                                    alpha
                                };
                                blend_pixel(
                                    target,
                                    x,
                                    y,
                                    src,
                                    (alpha as u32 * coverage / 255) as u8,
                                );
                            }
                        }
                    }
                }
                dirty = dirty.union(rect);
            }
            Ok(dirty)
        });
        self.cache = cache;
        result
    }

    fn target(&self, surface_id: u32) -> Result<Arc<RwLock<Canvas>>> {
        self.surfaces
            .read()
            .expect("surfaces lock poisoned")
            .get(&surface_id)
            .cloned()
            .ok_or_else(|| Error::Spice(format!("unknown surface {surface_id}")))
    }

    fn draw<F>(&self, base: &Base, cache: &mut ImageCache, f: F) -> Result<Vec<DisplayUpdate>>
    where
        F: FnOnce(&mut Canvas, &HashMap<u32, Arc<RwLock<Canvas>>>, &mut ImageCache) -> Result<Rect>,
    {
        let target = self.target(base.surface_id)?;
        let mut canvas = target.write().expect("canvas lock poisoned");
        let surfaces = self.surfaces.read().expect("surfaces lock poisoned");
        let dirty = f(&mut canvas, &surfaces, cache)?;
        drop(surfaces);
        let dirty = dirty.clamp(canvas.width, canvas.height);
        if dirty.is_empty() {
            Ok(Vec::new())
        } else {
            Ok(vec![DisplayUpdate::Dirty(dirty)])
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum DrawMode {
    Copy,
    Opaque,
    Blend,
    Rop3,
    Transparent { src_color: u32 },
    AlphaBlend { flags: u8, alpha: u8 },
}

#[derive(Debug, Clone)]
struct Base {
    surface_id: u32,
    rect: Rect,
    clip: Option<Vec<Rect>>,
}

#[derive(Debug, Clone)]
enum Brush {
    None,
    Solid(u32),
    Pattern { offset: usize, pos: (i32, i32) },
}

#[derive(Debug, Clone)]
struct Mask {
    flags: u8,
    pos: (i32, i32),
    offset: usize,
}

fn parse_base(cur: &mut Cursor<'_>) -> Result<Base> {
    let surface_id = cur.u32()?;
    let rect = parse_rect(cur)?;
    let clip_type = cur.u8()?;
    let clip = if clip_type == CLIP_TYPE_RECTS {
        let count = cur.u32()? as usize;
        if count > 65536 {
            return Err(Error::Spice("unreasonable clip rectangle count".into()));
        }
        let mut rects = Vec::with_capacity(count);
        for _ in 0..count {
            rects.push(parse_rect(cur)?);
        }
        Some(rects)
    } else {
        None
    };
    Ok(Base {
        surface_id,
        rect,
        clip,
    })
}

fn parse_rect(cur: &mut Cursor<'_>) -> Result<Rect> {
    let top = cur.i32()?;
    let left = cur.i32()?;
    let bottom = cur.i32()?;
    let right = cur.i32()?;
    Ok(Rect::from_wire(top, left, bottom, right))
}

fn parse_brush(cur: &mut Cursor<'_>) -> Result<Brush> {
    Ok(match cur.u8()? {
        BRUSH_TYPE_NONE => Brush::None,
        BRUSH_TYPE_SOLID => Brush::Solid(cur.u32()?),
        BRUSH_TYPE_PATTERN => {
            let offset = cur.u32()? as usize;
            let x = cur.i32()?;
            let y = cur.i32()?;
            Brush::Pattern {
                offset,
                pos: (x, y),
            }
        }
        other => return Err(Error::Spice(format!("unknown brush type {other}"))),
    })
}

fn parse_mask(cur: &mut Cursor<'_>) -> Result<Option<Mask>> {
    let flags = cur.u8()?;
    let x = cur.i32()?;
    let y = cur.i32()?;
    let offset = cur.u32()? as usize;
    if offset == 0 {
        Ok(None)
    } else {
        Ok(Some(Mask {
            flags,
            pos: (x, y),
            offset,
        }))
    }
}

fn clip_rects(base: &Base, target: &Canvas) -> Vec<Rect> {
    let full = base.rect.clamp(target.width, target.height);
    if full.is_empty() {
        return Vec::new();
    }
    match &base.clip {
        None => vec![full],
        Some(rects) => rects
            .iter()
            .map(|r| r.clamp(target.width, target.height).intersect(full))
            .filter(|r| !r.is_empty())
            .collect(),
    }
}

fn map_source(box_rect: &Rect, src: &Rect, image: &DecodedImage, x: i32, y: i32) -> (i32, i32) {
    if box_rect.w <= 0 || box_rect.h <= 0 {
        return (0, 0);
    }
    let src_w = if src.w > 0 { src.w } else { image.width as i32 };
    let src_h = if src.h > 0 {
        src.h
    } else {
        image.height as i32
    };
    let u = (x - box_rect.x) as i64;
    let v = (y - box_rect.y) as i64;
    let sx = src.x as i64 + u * src_w as i64 / box_rect.w.max(1) as i64;
    let sy = src.y as i64 + v * src_h as i64 / box_rect.h.max(1) as i64;
    (sx as i32, sy as i32)
}

fn mask_coverage(
    mask: Option<&Mask>,
    surfaces: &HashMap<u32, Arc<RwLock<Canvas>>>,
    cache: &mut ImageCache,
    target_id: u32,
    target: &Canvas,
    body: &[u8],
    at: (i32, i32),
) -> Result<u8> {
    let Some(mask) = mask else {
        return Ok(255);
    };
    let image = decode_image(body, mask.offset, cache, surfaces, target_id, target)?;
    let Some(image) = image else {
        return Ok(255);
    };
    let sample = image.sample(at.0 - mask.pos.0, at.1 - mask.pos.1);
    let mut coverage = if image.has_alpha {
        sample[3]
    } else {
        sample[0].max(sample[1]).max(sample[2])
    };
    if mask.flags & 1 != 0 {
        coverage = 255 - coverage;
    }
    Ok(coverage)
}

fn color_from_u32(color: u32) -> [u8; 4] {
    [
        ((color >> 16) & 0xff) as u8,
        ((color >> 8) & 0xff) as u8,
        (color & 0xff) as u8,
        255,
    ]
}

fn pattern_color(image: &DecodedImage, x: i32, y: i32) -> [u8; 4] {
    if image.width == 0 || image.height == 0 {
        return [0, 0, 0, 255];
    }
    let x = x.rem_euclid(image.width as i32);
    let y = y.rem_euclid(image.height as i32);
    image.sample(x, y)
}

fn blend_pixel(target: &mut Canvas, x: i32, y: i32, src: [u8; 4], coverage: u8) {
    if coverage == 255 {
        target.set(x, y, [src[0], src[1], src[2], 255]);
    } else if coverage > 0 {
        let alpha = coverage as u32;
        let dst = target.get(x, y);
        let inv = 255 - alpha;
        target.set(
            x,
            y,
            [
                ((src[0] as u32 * alpha + dst[0] as u32 * inv) / 255) as u8,
                ((src[1] as u32 * alpha + dst[1] as u32 * inv) / 255) as u8,
                ((src[2] as u32 * alpha + dst[2] as u32 * inv) / 255) as u8,
                255,
            ],
        );
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.data.len() {
            return Err(Error::Spice("truncated display message".into()));
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_i32(out: &mut Vec<u8>, value: i32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_rect(out: &mut Vec<u8>, top: i32, left: i32, bottom: i32, right: i32) {
        push_i32(out, top);
        push_i32(out, left);
        push_i32(out, bottom);
        push_i32(out, right);
    }

    fn mode(width: u32, height: u32) -> Vec<u8> {
        let mut body = Vec::new();
        push_u32(&mut body, width);
        push_u32(&mut body, height);
        push_u32(&mut body, 32);
        body
    }

    #[test]
    fn wire_rect_order_is_top_left_bottom_right() {
        let mut body = Vec::new();
        push_rect(&mut body, 2, 3, 6, 9);
        let mut cur = Cursor::new(&body);
        let rect = parse_rect(&mut cur).unwrap();
        assert_eq!(rect, Rect::new(3, 2, 6, 4));
    }

    #[test]
    fn renders_solid_fill() {
        let mut renderer = DisplayRenderer::new();
        renderer.handle(MSG_DISPLAY_MODE, &mode(20, 10)).unwrap();

        let mut body = Vec::new();
        push_u32(&mut body, 0);
        push_rect(&mut body, 2, 3, 6, 9);
        body.push(CLIP_TYPE_NONE);
        body.push(BRUSH_TYPE_SOLID);
        push_u32(&mut body, 0x0011_2233);
        body.extend_from_slice(&0u16.to_le_bytes());
        body.push(0); // mask flags
        push_i32(&mut body, 0);
        push_i32(&mut body, 0);
        push_u32(&mut body, 0); // no mask

        let updates = renderer.handle(MSG_DISPLAY_DRAW_FILL, &body).unwrap();
        assert!(updates
            .iter()
            .any(|update| matches!(update, DisplayUpdate::Dirty(_))));

        let primary = renderer.primary().unwrap();
        let canvas = primary.read().unwrap();
        assert_eq!(canvas.get(3, 2), [0x11, 0x22, 0x33, 255]);
        assert_eq!(canvas.get(8, 5), [0x11, 0x22, 0x33, 255]);
        assert_eq!(canvas.get(2, 2), [0, 0, 0, 255]);
        assert_eq!(canvas.get(9, 6), [0, 0, 0, 255]);
    }

    #[test]
    fn renders_bitmap_copy() {
        let mut renderer = DisplayRenderer::new();
        renderer.handle(MSG_DISPLAY_MODE, &mode(4, 4)).unwrap();

        let mut body = Vec::new();
        push_u32(&mut body, 0);
        push_rect(&mut body, 0, 0, 2, 2);
        body.push(CLIP_TYPE_NONE);
        let offset_position = body.len();
        push_u32(&mut body, 0); // image offset, patched below
        push_rect(&mut body, 0, 0, 2, 2); // source area
        body.extend_from_slice(&0u16.to_le_bytes()); // rop
        body.push(0); // scale
        body.push(0); // mask flags
        push_i32(&mut body, 0);
        push_i32(&mut body, 0);
        push_u32(&mut body, 0); // no mask

        while body.len() % 4 != 0 {
            body.push(0);
        }
        let image_offset = body.len() as u32;
        body[offset_position..offset_position + 4].copy_from_slice(&image_offset.to_le_bytes());

        body.extend_from_slice(&0u64.to_le_bytes()); // cache id
        body.push(IMAGE_TYPE_BITMAP);
        body.push(0); // image flags
        push_u32(&mut body, 2); // width
        push_u32(&mut body, 2); // height
        body.push(BITMAP_FMT_32BIT);
        body.push(0); // bitmap flags
        push_u32(&mut body, 0); // x
        push_u32(&mut body, 0); // y
        push_i32(&mut body, 8); // stride
        push_u32(&mut body, 0); // no palette

        // BGRX rows are bottom-up by default: blue, white / red, green.
        body.extend_from_slice(&[255, 0, 0, 0, 255, 255, 255, 0]);
        body.extend_from_slice(&[0, 0, 255, 0, 0, 255, 0, 0]);

        renderer.handle(MSG_DISPLAY_DRAW_COPY, &body).unwrap();

        let primary = renderer.primary().unwrap();
        let canvas = primary.read().unwrap();
        assert_eq!(canvas.get(0, 0), [255, 0, 0, 255]);
        assert_eq!(canvas.get(1, 0), [0, 255, 0, 255]);
        assert_eq!(canvas.get(0, 1), [0, 0, 255, 255]);
        assert_eq!(canvas.get(1, 1), [255, 255, 255, 255]);
        assert_eq!(canvas.get(2, 0), [0, 0, 0, 255]);
    }
}
