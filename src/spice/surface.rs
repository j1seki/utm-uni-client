//! Off-screen SPICE surfaces stored as RGBA8.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn from_wire(top: i32, left: i32, bottom: i32, right: i32) -> Self {
        Self {
            x: left,
            y: top,
            w: right.saturating_sub(left),
            h: bottom.saturating_sub(top),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub fn intersect(&self, other: Rect) -> Rect {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.w).min(other.x + other.w);
        let y1 = (self.y + self.h).min(other.y + other.h);
        Rect::new(x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
    }

    pub fn union(&self, other: Rect) -> Rect {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return *self;
        }
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = (self.x + self.w).max(other.x + other.w);
        let y1 = (self.y + self.h).max(other.y + other.h);
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    pub fn clamp(&self, width: u32, height: u32) -> Rect {
        self.intersect(Rect::new(0, 0, width as i32, height as i32))
    }
}

#[derive(Debug, Clone)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    /// RGBA8, row-major, `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Self {
        let len = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        Self {
            width,
            height,
            pixels: vec![0u8; len],
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if self.width == width && self.height == height {
            return;
        }
        *self = Canvas::new(width, height);
    }

    pub fn clear(&mut self, rgba: [u8; 4]) {
        for chunk in self.pixels.chunks_exact_mut(4) {
            chunk.copy_from_slice(&rgba);
        }
    }

    #[inline]
    pub fn set(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let index = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        self.pixels[index..index + 4].copy_from_slice(&rgba);
    }

    #[inline]
    pub fn get(&self, x: i32, y: i32) -> [u8; 4] {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return [0, 0, 0, 255];
        }
        let index = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        [
            self.pixels[index],
            self.pixels[index + 1],
            self.pixels[index + 2],
            self.pixels[index + 3],
        ]
    }

    #[inline]
    pub fn blend(&mut self, x: i32, y: i32, src: [u8; 4]) {
        let alpha = src[3] as u32;
        if alpha == 255 {
            self.set(x, y, src);
            return;
        }
        if alpha == 0 {
            return;
        }
        let dst = self.get(x, y);
        let inv = 255 - alpha;
        let out = [
            ((src[0] as u32 * alpha + dst[0] as u32 * inv) / 255) as u8,
            ((src[1] as u32 * alpha + dst[1] as u32 * inv) / 255) as u8,
            ((src[2] as u32 * alpha + dst[2] as u32 * inv) / 255) as u8,
            ((alpha + dst[3] as u32 * inv / 255).min(255)) as u8,
        ];
        self.set(x, y, out);
    }

    /// Copy a region of this canvas into another rectangle of the same canvas.
    pub fn copy_within(&mut self, dst: Rect, src_x: i32, src_y: i32) {
        if dst.is_empty() {
            return;
        }
        let dst = dst.clamp(self.width, self.height);
        let mut temp = Vec::with_capacity((dst.w.max(0) * dst.h.max(0) * 4) as usize);
        for row in 0..dst.h {
            for col in 0..dst.w {
                temp.extend_from_slice(&self.get(src_x + col, src_y + row));
            }
        }
        let mut index = 0;
        for row in 0..dst.h {
            for col in 0..dst.w {
                self.set(
                    dst.x + col,
                    dst.y + row,
                    [
                        temp[index],
                        temp[index + 1],
                        temp[index + 2],
                        temp[index + 3],
                    ],
                );
                index += 4;
            }
        }
    }
}
