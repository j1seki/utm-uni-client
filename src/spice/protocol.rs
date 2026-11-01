//! SPICE protocol constants (subset used by the web client).

pub const SPICE_MAGIC: &[u8; 4] = b"REDQ";
pub const SPICE_VERSION_MAJOR: u32 = 2;
pub const SPICE_VERSION_MINOR: u32 = 2;

pub const LINK_HEADER_SIZE: usize = 16;
pub const LINK_MESS_SIZE: usize = 18;
pub const LINK_REPLY_FIXED_SIZE: usize = 178;
pub const TICKET_PUBKEY_BYTES: usize = 162;
pub const TICKET_SIZE: usize = 128;

pub const CHANNEL_MAIN: u8 = 1;
pub const CHANNEL_DISPLAY: u8 = 2;
pub const CHANNEL_INPUTS: u8 = 3;

// Common capability bit positions.
pub const COMMON_CAP_PROTOCOL_AUTH_SELECTION: u32 = 0;
pub const COMMON_CAP_AUTH_SPICE: u32 = 1;
pub const COMMON_CAP_MINI_HEADER: u32 = 3;

// Auth mechanism values (these happen to use the capability bit index).
pub const AUTH_MECHANISM_SPICE: u32 = 1;

// Display capability bit positions.
pub const DISPLAY_CAP_MONITORS_CONFIG: u32 = 1;
pub const DISPLAY_CAP_PREF_COMPRESSION: u32 = 6;

// Main capability bit positions.
pub const MAIN_CAP_NAME_AND_UUID: u32 = 1;

// Inputs capability bit positions.
pub const INPUTS_CAP_KEY_SCANCODE: u32 = 0;

// Base channel messages (server -> client).
pub const MSG_MIGRATE: u16 = 1;
pub const MSG_SET_ACK: u16 = 3;
pub const MSG_PING: u16 = 4;
pub const MSG_WAIT_FOR_CHANNELS: u16 = 5;
pub const MSG_DISCONNECTING: u16 = 6;
pub const MSG_NOTIFY: u16 = 7;
pub const MSG_LIST: u16 = 8;

// Base channel messages (client -> server).
pub const MSGC_ACK_SYNC: u16 = 1;
pub const MSGC_ACK: u16 = 2;
pub const MSGC_PONG: u16 = 3;
pub const MSGC_DISCONNECTING: u16 = 6;

// Main channel messages.
pub const MSG_MAIN_INIT: u16 = 103;
pub const MSG_MAIN_CHANNELS_LIST: u16 = 104;
pub const MSG_MAIN_MOUSE_MODE: u16 = 105;
pub const MSG_MAIN_AGENT_CONNECTED: u16 = 107;
pub const MSG_MAIN_AGENT_DISCONNECTED: u16 = 108;
pub const MSG_MAIN_NAME: u16 = 113;
pub const MSG_MAIN_UUID: u16 = 114;

pub const MSGC_MAIN_ATTACH_CHANNELS: u16 = 104;
pub const MSGC_MAIN_MOUSE_MODE_REQUEST: u16 = 105;
pub const MSGC_MAIN_AGENT_START: u16 = 106;

// Display channel messages.
pub const MSG_DISPLAY_MODE: u16 = 101;
pub const MSG_DISPLAY_MARK: u16 = 102;
pub const MSG_DISPLAY_RESET: u16 = 103;
pub const MSG_DISPLAY_COPY_BITS: u16 = 104;
pub const MSG_DISPLAY_INVAL_LIST: u16 = 105;
pub const MSG_DISPLAY_INVAL_ALL_PIXMAPS: u16 = 106;
pub const MSG_DISPLAY_INVAL_PALETTE: u16 = 107;
pub const MSG_DISPLAY_INVAL_ALL_PALETTES: u16 = 108;
pub const MSG_DISPLAY_STREAM_CREATE: u16 = 122;
pub const MSG_DISPLAY_STREAM_DATA: u16 = 123;
pub const MSG_DISPLAY_STREAM_CLIP: u16 = 124;
pub const MSG_DISPLAY_STREAM_DESTROY: u16 = 125;
pub const MSG_DISPLAY_STREAM_DESTROY_ALL: u16 = 126;
pub const MSG_DISPLAY_DRAW_FILL: u16 = 302;
pub const MSG_DISPLAY_DRAW_OPAQUE: u16 = 303;
pub const MSG_DISPLAY_DRAW_COPY: u16 = 304;
pub const MSG_DISPLAY_DRAW_BLEND: u16 = 305;
pub const MSG_DISPLAY_DRAW_BLACKNESS: u16 = 306;
pub const MSG_DISPLAY_DRAW_WHITENESS: u16 = 307;
pub const MSG_DISPLAY_DRAW_INVERS: u16 = 308;
pub const MSG_DISPLAY_DRAW_ROP3: u16 = 309;
pub const MSG_DISPLAY_DRAW_STROKE: u16 = 310;
pub const MSG_DISPLAY_DRAW_TEXT: u16 = 311;
pub const MSG_DISPLAY_DRAW_TRANSPARENT: u16 = 312;
pub const MSG_DISPLAY_DRAW_ALPHA_BLEND: u16 = 313;
pub const MSG_DISPLAY_SURFACE_CREATE: u16 = 314;
pub const MSG_DISPLAY_SURFACE_DESTROY: u16 = 315;
pub const MSG_DISPLAY_STREAM_DATA_SIZED: u16 = 316;
pub const MSG_DISPLAY_MONITORS_CONFIG: u16 = 317;
pub const MSG_DISPLAY_DRAW_COMPOSITE: u16 = 318;
pub const MSG_DISPLAY_STREAM_ACTIVATE_REPORT: u16 = 319;

pub const MSGC_DISPLAY_INIT: u16 = 101;
pub const MSGC_DISPLAY_STREAM_REPORT: u16 = 102;
pub const MSGC_DISPLAY_PREFERRED_COMPRESSION: u16 = 103;
pub const MSGC_DISPLAY_GL_DRAW_DONE: u16 = 104;
pub const MSGC_DISPLAY_PREFERRED_VIDEO_CODEC_TYPE: u16 = 105;

// Inputs channel messages.
pub const MSG_INPUTS_INIT: u16 = 101;
pub const MSG_INPUTS_KEY_MODIFIERS: u16 = 102;
pub const MSG_INPUTS_MOUSE_MOTION_ACK: u16 = 111;

pub const MSGC_INPUTS_KEY_DOWN: u16 = 101;
pub const MSGC_INPUTS_KEY_UP: u16 = 102;
pub const MSGC_INPUTS_KEY_MODIFIERS: u16 = 103;
pub const MSGC_INPUTS_KEY_SCANCODE: u16 = 104;
pub const MSGC_INPUTS_MOUSE_MOTION: u16 = 111;
pub const MSGC_INPUTS_MOUSE_POSITION: u16 = 112;
pub const MSGC_INPUTS_MOUSE_PRESS: u16 = 113;
pub const MSGC_INPUTS_MOUSE_RELEASE: u16 = 114;

// Image types.
pub const IMAGE_TYPE_BITMAP: u8 = 0;
pub const IMAGE_TYPE_QUIC: u8 = 1;
pub const IMAGE_TYPE_LZ_PLT: u8 = 100;
pub const IMAGE_TYPE_LZ_RGB: u8 = 101;
pub const IMAGE_TYPE_GLZ_RGB: u8 = 102;
pub const IMAGE_TYPE_FROM_CACHE: u8 = 103;
pub const IMAGE_TYPE_SURFACE: u8 = 104;
pub const IMAGE_TYPE_JPEG: u8 = 105;
pub const IMAGE_TYPE_FROM_CACHE_LOSSLESS: u8 = 106;
pub const IMAGE_TYPE_ZLIB_GLZ_RGB: u8 = 107;
pub const IMAGE_TYPE_JPEG_ALPHA: u8 = 108;
pub const IMAGE_TYPE_LZ4: u8 = 109;

// Bitmap formats.
pub const BITMAP_FMT_1BIT_LE: u8 = 1;
pub const BITMAP_FMT_1BIT_BE: u8 = 2;
pub const BITMAP_FMT_4BIT_LE: u8 = 3;
pub const BITMAP_FMT_4BIT_BE: u8 = 4;
pub const BITMAP_FMT_8BIT: u8 = 5;
pub const BITMAP_FMT_16BIT: u8 = 6;
pub const BITMAP_FMT_24BIT: u8 = 7;
pub const BITMAP_FMT_32BIT: u8 = 8;
pub const BITMAP_FMT_RGBA: u8 = 9;
pub const BITMAP_FMT_8BIT_A: u8 = 10;

pub const BITMAP_FLAG_PAL_CACHE_ME: u8 = 1 << 0;
pub const BITMAP_FLAG_PAL_FROM_CACHE: u8 = 1 << 1;
pub const BITMAP_FLAG_TOP_DOWN: u8 = 1 << 2;

pub const IMAGE_FLAG_CACHE_ME: u8 = 1 << 0;
pub const IMAGE_FLAG_HIGH_BITS_SET: u8 = 1 << 1;
pub const IMAGE_FLAG_CACHE_REPLACE_ME: u8 = 1 << 2;

pub const CLIP_TYPE_NONE: u8 = 0;
pub const CLIP_TYPE_RECTS: u8 = 1;

pub const BRUSH_TYPE_NONE: u8 = 0;
pub const BRUSH_TYPE_SOLID: u8 = 1;
pub const BRUSH_TYPE_PATTERN: u8 = 2;

pub const SURFACE_FMT_32_XRGB: u32 = 32;
pub const SURFACE_FMT_32_ARGB: u32 = 96;
pub const SURFACE_FLAG_PRIMARY: u32 = 1;

pub const MOUSE_MODE_SERVER: u16 = 1;
pub const MOUSE_MODE_CLIENT: u16 = 2;

pub const IMAGE_COMPRESSION_OFF: u8 = 1;

pub fn has_cap(caps: &[u32], bit: u32) -> bool {
    let word = (bit / 32) as usize;
    caps.get(word).is_some_and(|w| (w >> (bit % 32)) & 1 != 0)
}

pub fn make_scancode(scancode: u32, release: bool) -> u32 {
    let mut scancode = scancode & 0x37f;
    if release {
        scancode |= 0x80;
    }
    if scancode < 0x100 {
        scancode
    } else {
        0xe0 | ((scancode - 0x100) << 8)
    }
}
