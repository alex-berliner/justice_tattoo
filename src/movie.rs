//! The JTM1 movie blob: parsing, on-demand frame decoding, and `movie` flash
//! partition I/O.
//!
//! A movie is a single JTM1 blob (see PLAN.md §4). It can live in two places:
//!   * the `movie` flash partition - a movie uploaded over Wi-Fi, or
//!   * `.rodata` - the default movie baked in at compile time by `build.rs`.
//!
//! `Movie::load()` prefers a valid blob in the partition and falls back to the
//! baked-in default. Frames are read from flash on demand (no 2 MB mmap): only
//! the small header is held in RAM. `MovieWriter` streams an upload into the
//! partition, erasing each 4 KB sector just before it is written.

use anyhow::{anyhow, bail, Result};
use esp_idf_svc::sys;

use crate::ssd1309::FRAME_BYTES;

/// 4 KB flash sector - the erase granularity and our write-block size.
const SECTOR: usize = 4096;

/// JTM1 magic at offset 0.
const MAGIC: [u8; 4] = *b"JTM1";

/// Fixed header: magic(4) + format(1) + reserved(1) + frame_count(2).
const HEADER_FIXED: usize = 8;

/// Frames are clamped to this minimum so a movie can never spin too fast.
const MIN_DELAY_MS: u16 = 30;

/// Sanity cap on the frame count parsed from a (possibly corrupt) header, so a
/// bad blob cannot drive an enormous header allocation.
const MAX_FRAMES: usize = 8192;

/// The default movie, baked in by `build.rs` as a JTM1 blob.
static DEFAULT_MOVIE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/default_movie.bin"));

/// Where the movie bytes physically live.
enum Store {
    /// A `.rodata` slice - the baked-in default movie.
    Embedded(&'static [u8]),
    /// The `movie` flash partition, read on demand.
    Partition(*const sys::esp_partition_t),
}

impl Store {
    /// Total readable size of the backing store, in bytes.
    fn len(&self) -> usize {
        match self {
            Store::Embedded(blob) => blob.len(),
            Store::Partition(part) => unsafe { (**part).size as usize },
        }
    }

    /// Copy `buf.len()` bytes starting at `offset` into `buf`.
    fn read_into(&self, offset: usize, buf: &mut [u8]) -> Result<()> {
        match self {
            Store::Embedded(blob) => {
                let src = blob
                    .get(offset..offset + buf.len())
                    .ok_or_else(|| anyhow!("read past end of embedded movie"))?;
                buf.copy_from_slice(src);
                Ok(())
            }
            Store::Partition(part) => {
                let err = unsafe {
                    sys::esp_partition_read(
                        *part,
                        offset,
                        buf.as_mut_ptr() as *mut core::ffi::c_void,
                        buf.len(),
                    )
                };
                esp_check(err, "read")
            }
        }
    }
}

/// A parsed movie: header in RAM, frame data read from `store` on demand.
pub struct Movie {
    store: Store,
    format: u8,
    frame_count: usize,
    delays: Vec<u16>,
    /// `frame_count + 1` offsets into the data section; frame `i` is
    /// `[offsets[i]..offsets[i + 1]]`.
    offsets: Vec<u32>,
    /// Byte offset where frame data begins (just past the variable header).
    data_start: usize,
    max_frame_len: usize,
}

impl Movie {
    /// Load the movie to play: a valid blob in the `movie` partition if there
    /// is one, otherwise the baked-in default.
    pub fn load() -> Movie {
        if let Some(part) = find_movie_partition() {
            match Movie::parse(Store::Partition(part)) {
                Ok(m) => {
                    log::info!(
                        "movie: uploaded blob in flash partition, {} frames, format {}",
                        m.frame_count,
                        m.format,
                    );
                    return m;
                }
                Err(e) => {
                    log::info!("movie: no valid blob in partition ({e}); using default");
                }
            }
        }
        let m = Movie::parse(Store::Embedded(DEFAULT_MOVIE))
            .expect("baked-in default movie must be a valid JTM1 blob");
        log::info!("movie: baked-in default, {} frames, format {}", m.frame_count, m.format);
        m
    }

    /// Parse and validate a JTM1 blob from `store`.
    fn parse(store: Store) -> Result<Movie> {
        let mut fixed = [0u8; HEADER_FIXED];
        store.read_into(0, &mut fixed)?;
        if fixed[0..4] != MAGIC {
            bail!("bad magic");
        }
        let format = fixed[4];
        let frame_count = u16::from_le_bytes([fixed[6], fixed[7]]) as usize;
        if frame_count == 0 {
            bail!("zero frames");
        }
        if frame_count > MAX_FRAMES {
            bail!("implausible frame count {frame_count}");
        }

        // Variable header: delays (2 * N) then offsets (4 * (N + 1)).
        let delays_len = 2 * frame_count;
        let offsets_len = 4 * (frame_count + 1);
        let mut vh = vec![0u8; delays_len + offsets_len];
        store.read_into(HEADER_FIXED, &mut vh)?;

        let mut delays = Vec::with_capacity(frame_count);
        for i in 0..frame_count {
            delays.push(u16::from_le_bytes([vh[2 * i], vh[2 * i + 1]]));
        }
        let mut offsets = Vec::with_capacity(frame_count + 1);
        for i in 0..=frame_count {
            let b = delays_len + 4 * i;
            offsets.push(u32::from_le_bytes([vh[b], vh[b + 1], vh[b + 2], vh[b + 3]]));
        }
        if offsets.windows(2).any(|w| w[1] < w[0]) {
            bail!("non-monotonic frame offsets");
        }

        let data_start = HEADER_FIXED + delays_len + offsets_len;
        let data_len = *offsets.last().unwrap() as usize;
        if data_start + data_len > store.len() {
            bail!("frame data runs past the end of the blob");
        }
        let max_frame_len = offsets
            .windows(2)
            .map(|w| (w[1] - w[0]) as usize)
            .max()
            .unwrap_or(0);
        if format == 0 && max_frame_len != FRAME_BYTES {
            bail!("raw frame is not {FRAME_BYTES} bytes");
        }
        if format > 1 {
            bail!("unknown format {format}");
        }

        Ok(Movie {
            store,
            format,
            frame_count,
            delays,
            offsets,
            data_start,
            max_frame_len,
        })
    }

    pub fn frame_count(&self) -> usize {
        self.frame_count
    }

    /// Frame `i`'s on-screen duration, clamped to a sane minimum.
    pub fn delay_ms(&self, i: usize) -> u32 {
        self.delays[i].max(MIN_DELAY_MS) as u32
    }

    /// Size the playback scratch buffer: the largest encoded frame.
    pub fn max_frame_len(&self) -> usize {
        self.max_frame_len
    }

    /// Render frame `i` into `framebuf`. `scratch` must be at least
    /// `max_frame_len()` bytes; it holds the encoded frame for delta formats.
    pub fn render_frame(
        &self,
        i: usize,
        framebuf: &mut [u8; FRAME_BYTES],
        scratch: &mut [u8],
    ) -> Result<()> {
        let start = self.data_start + self.offsets[i] as usize;
        let len = (self.offsets[i + 1] - self.offsets[i]) as usize;
        match self.format {
            // Raw: the frame is the verbatim page-packed framebuffer.
            0 => self.store.read_into(start, &mut framebuf[..])?,
            // XOR-delta + RLE: frame 0 is a delta against zero, so the running
            // buffer resets at the loop start; later frames build on it.
            1 => {
                if i == 0 {
                    framebuf.fill(0);
                }
                let encoded = &mut scratch[..len];
                self.store.read_into(start, encoded)?;
                apply_delta_rle(encoded, framebuf);
            }
            other => bail!("unknown format {other}"),
        }
        Ok(())
    }
}

/// Apply one RLE-encoded XOR delta to `buf` in place.
///
/// The op stream is 2-byte little-endian headers: bit 15 = type (0 = COPY of an
/// unchanged run, 1 = XOR of a literal run), bits 0..14 = run length. XOR ops
/// are followed by `len` literal bytes that are XORed into `buf`. The length
/// clamps guard against a malformed blob - well-formed data never triggers them.
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

/// A streaming writer for the `movie` partition. It buffers incoming bytes into
/// 4 KB blocks; each block's flash sector is erased immediately before it is
/// written, so only the bytes the movie actually uses are touched.
pub struct MovieWriter {
    part: *const sys::esp_partition_t,
    capacity: usize,
    block: [u8; SECTOR],
    block_len: usize,
    /// 4 KB-aligned offset of `block` within the partition.
    flash_pos: usize,
}

impl MovieWriter {
    /// Open the `movie` partition for writing.
    pub fn new() -> Result<MovieWriter> {
        let part =
            find_movie_partition().ok_or_else(|| anyhow!("'movie' partition not found"))?;
        let capacity = unsafe { (*part).size as usize };
        Ok(MovieWriter {
            part,
            capacity,
            block: [0u8; SECTOR],
            block_len: 0,
            flash_pos: 0,
        })
    }

    /// Append `data` to the partition, flushing full 4 KB blocks as they fill.
    pub fn write(&mut self, mut data: &[u8]) -> Result<()> {
        while !data.is_empty() {
            let take = (SECTOR - self.block_len).min(data.len());
            self.block[self.block_len..self.block_len + take]
                .copy_from_slice(&data[..take]);
            self.block_len += take;
            data = &data[take..];
            if self.block_len == SECTOR {
                self.flush_block()?;
            }
        }
        Ok(())
    }

    /// Erase + write the current block (zero-padding a partial final block).
    fn flush_block(&mut self) -> Result<()> {
        if self.flash_pos + SECTOR > self.capacity {
            bail!("movie exceeds the {}-byte partition", self.capacity);
        }
        for b in &mut self.block[self.block_len..] {
            *b = 0;
        }
        unsafe {
            esp_check(
                sys::esp_partition_erase_range(self.part, self.flash_pos, SECTOR),
                "erase",
            )?;
            esp_check(
                sys::esp_partition_write(
                    self.part,
                    self.flash_pos,
                    self.block.as_ptr() as *const core::ffi::c_void,
                    SECTOR,
                ),
                "write",
            )?;
        }
        self.flash_pos += SECTOR;
        self.block_len = 0;
        Ok(())
    }

    /// Flush the final partial block. Returns total bytes written.
    pub fn finish(mut self) -> Result<usize> {
        let total = self.flash_pos + self.block_len;
        if self.block_len > 0 {
            self.flush_block()?;
        }
        Ok(total)
    }
}

/// Locate the `movie` data partition by label.
fn find_movie_partition() -> Option<*const sys::esp_partition_t> {
    let label = b"movie\0";
    let part = unsafe {
        sys::esp_partition_find_first(
            sys::esp_partition_type_t_ESP_PARTITION_TYPE_DATA,
            sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_ANY,
            label.as_ptr() as *const core::ffi::c_char,
        )
    };
    if part.is_null() {
        None
    } else {
        Some(part)
    }
}

/// Turn an `esp_err_t` into a `Result`.
fn esp_check(err: sys::esp_err_t, what: &str) -> Result<()> {
    if err == sys::ESP_OK {
        Ok(())
    } else {
        Err(anyhow!("esp_partition_{what} failed: {err}"))
    }
}
