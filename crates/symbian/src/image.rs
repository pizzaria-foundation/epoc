//! Image decoding, through the device's own codecs.
//!
//! The shim wraps `CImageDecoder`, which is whatever plugins the handset has — JPEG,
//! PNG, GIF and BMP on every S60 3rd Edition device. Notably *not* WebP, which
//! postdates the platform by two years; a Telegram sticker therefore cannot be decoded
//! here at all, and the caller has to recognise that rather than discover it as a
//! failed decode.
//!
//! # Why this is a state machine and not a function
//!
//! Decoding is asynchronous on the device and it has to be. `CImageDecoder::Convert`
//! is driven by an active object in the calling thread, and the calling thread is the
//! one running the UI, so waiting for the result deadlocks it — see the long note in
//! `shim/src/shim_image.cpp`. The completion arrives as `SHIM_EV_IMAGE_DONE`, like a
//! socket read or a timer, and this module is the bookkeeping between starting a
//! decode and having pixels.
//!
//! # The size you ask for is not the size you get
//!
//! The ICL scales only by powers of two, and only by the factors the codec supports.
//! [`Decoder::start`] takes a box to fit inside and the result is the largest reduction
//! that fits — never bigger than the box, frequently smaller, and for PNG usually not
//! reduced at all. Fitting exactly is the caller's job, with a resampling blit.

use alloc::vec;
use alloc::vec::Vec;

use symbian_sys as sys;

use crate::error::{Error, Result};
use crate::fs::Utf16Path;

/// A decoded image: RGB565 pixels, tightly packed, `width * height` of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub pixels: Vec<u16>,
    pub width: i32,
    pub height: i32,
}

impl Image {
    /// The row at `y`, or `None` past the bottom. Bounds-checked here so drawing code
    /// does not have to trust `width` and `height` against `pixels.len()` — a
    /// mismatch there is a panic on a device where a panic is a silent vanish.
    pub fn row(&self, y: i32) -> Option<&[u16]> {
        if y < 0 || y >= self.height || self.width <= 0 {
            return None;
        }
        let start = (y as usize).checked_mul(self.width as usize)?;
        let end = start.checked_add(self.width as usize)?;
        self.pixels.get(start..end)
    }
}

/// What the shim can do with images. One implementation over the shim, one in memory
/// for tests — the same split as [`crate::fs::Fs`] and [`crate::net::Net`], and for the
/// same reason: the logic worth testing is the sequencing, not the FFI call.
pub trait Images {
    /// Dimensions without decoding. Cheap: header only.
    fn probe(&mut self, path: &Utf16Path) -> Result<(i32, i32)>;
    /// Begin decoding a file, fitting inside `max_w` × `max_h`.
    fn start_file(&mut self, path: &Utf16Path, max_w: i32, max_h: i32) -> Result<i32>;
    /// Begin decoding bytes already in memory.
    ///
    /// The buffer must stay alive and unmodified until the completion arrives: the
    /// decoder reads from it rather than copying. [`Decoder`] owns the bytes for
    /// exactly that reason.
    fn start_mem(&mut self, data: &[u8], max_w: i32, max_h: i32) -> Result<i32>;
    /// Collect the pixels once the completion said success.
    fn result(&mut self, handle: i32) -> Result<Image>;
    fn close(&mut self, handle: i32);
    /// What state a decode is in. See [`Progress`].
    ///
    /// Only useful for one thing, and it is worth the ABI: a decode that never completes
    /// emits no event, so without asking there is no evidence at all — not whether the
    /// image opened, not how big it is, not whether the request is still outstanding.
    fn describe(&mut self, _handle: i32) -> Option<Progress> {
        None
    }
}

/// A snapshot of a decode, for when one does not finish.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    /// True once the codec has reported, whether it succeeded or not.
    pub done: bool,
    /// The completion code, meaningful once `done`.
    pub error: i32,
    /// Frames the decoder found, or -1 if it never got that far — which distinguishes
    /// "the image did not open" from "the image opened and the decode is stuck".
    pub frames: i32,
    pub native_w: i32,
    pub native_h: i32,
    /// The power-of-two reduction chosen, or -1 if the decode never reached that point.
    pub factor: i32,
    /// The size of the bitmap actually created.
    pub out_w: i32,
    pub out_h: i32,
    /// Whether the request is still outstanding.
    pub active: bool,
    /// The `TDisplayMode` the destination bitmap was created in — the one the *frame*
    /// asked for, not the one this side would prefer.
    pub mode: i32,
    /// `TFrameInfo::iFlags`. Bit 2 is `EFullyScaleable`, bit 4 is `ECanDither`.
    pub frame_flags: i32,
    /// How many `ContinueConvert` rounds the decode has needed. Any at all is a fact
    /// about this handset's codec worth recording.
    pub continues: i32,
}

/// [`Images`] over the shim.
#[derive(Copy, Clone, Debug, Default)]
pub struct ShimImages;

impl Images for ShimImages {
    fn probe(&mut self, path: &Utf16Path) -> Result<(i32, i32)> {
        let units = path.as_units();
        let mut w = 0i32;
        let mut h = 0i32;
        Error::check(unsafe {
            sys::shim_image_probe(units.as_ptr(), units.len() as i32, &mut w, &mut h)
        })?;
        if w <= 0 || h <= 0 {
            return Err(Error::NotFound);
        }
        Ok((w, h))
    }

    fn start_file(&mut self, path: &Utf16Path, max_w: i32, max_h: i32) -> Result<i32> {
        let units = path.as_units();
        let mut handle = 0i32;
        Error::check(unsafe {
            sys::shim_image_decode_start(
                units.as_ptr(),
                units.len() as i32,
                max_w,
                max_h,
                &mut handle,
            )
        })?;
        Ok(handle)
    }

    fn start_mem(&mut self, data: &[u8], max_w: i32, max_h: i32) -> Result<i32> {
        if data.is_empty() {
            return Err(Error::Argument);
        }
        let mut handle = 0i32;
        Error::check(unsafe {
            sys::shim_image_decode_start_mem(
                data.as_ptr(),
                data.len() as i32,
                max_w,
                max_h,
                &mut handle,
            )
        })?;
        Ok(handle)
    }

    fn result(&mut self, handle: i32) -> Result<Image> {
        // Asked twice: once with no room, to learn the size the codec settled on, and
        // once with the buffer. The alternative is trusting the width and height the
        // event carried, which is the same number by a longer route and one more thing
        // for a caller to get wrong.
        let mut w = 0i32;
        let mut h = 0i32;
        let mut probe = [0u16; 1];
        let rc = unsafe { sys::shim_image_result(handle, probe.as_mut_ptr(), 1, &mut w, &mut h) };
        // Overflow is the expected answer and means the size fields are filled.
        if rc != sys::SHIM_ERR_OVERFLOW && rc != sys::SHIM_OK {
            return Err(Error::from_code(rc));
        }
        if w <= 0 || h <= 0 {
            return Err(Error::NotFound);
        }
        let count = (w as i64) * (h as i64);
        // i32 pixels is the ABI's own limit; the shim also refuses anything over a
        // megapixel, so this only catches a nonsense reply.
        if count <= 0 || count > i32::MAX as i64 {
            return Err(Error::Overflow);
        }

        let mut pixels = vec![0u16; count as usize];
        Error::check(unsafe {
            sys::shim_image_result(handle, pixels.as_mut_ptr(), count as i32, &mut w, &mut h)
        })?;
        Ok(Image { pixels, width: w, height: h })
    }

    fn close(&mut self, handle: i32) {
        unsafe { sys::shim_image_close(handle) }
    }

    fn describe(&mut self, handle: i32) -> Option<Progress> {
        let mut v = [0i32; 12];
        let rc = unsafe { sys::shim_image_describe(handle, v.as_mut_ptr(), v.len() as i32) };
        if rc != sys::SHIM_OK {
            return None;
        }
        Some(Progress {
            done: v[0] == 2,
            error: v[1],
            frames: v[2],
            native_w: v[3],
            native_h: v[4],
            factor: v[5],
            out_w: v[6],
            out_h: v[7],
            active: v[8] != 0,
            mode: v[9],
            frame_flags: v[10],
            continues: v[11],
        })
    }
}

/// One decode in flight.
///
/// Holds the source bytes for a memory decode, because the shim reads from them until
/// the completion arrives and nothing else is keeping them alive. Closes its handle on
/// drop, so an abandoned decode releases the slot and its bitmap instead of holding
/// both until the app exits.
pub struct Decoder<I: Images> {
    images: I,
    handle: i32,
    /// Kept alive for the decoder to read. Never touched here.
    _source: Option<Vec<u8>>,
}

impl<I: Images> Decoder<I> {
    /// Start decoding a file on disk.
    pub fn file(mut images: I, path: &Utf16Path, max_w: i32, max_h: i32) -> Result<Self> {
        let handle = images.start_file(path, max_w, max_h)?;
        Ok(Self { images, handle, _source: None })
    }

    /// Start decoding bytes, taking ownership of them for the duration.
    pub fn memory(mut images: I, data: Vec<u8>, max_w: i32, max_h: i32) -> Result<Self> {
        let handle = images.start_mem(&data, max_w, max_h)?;
        Ok(Self { images, handle, _source: Some(data) })
    }

    /// The handle the `SHIM_EV_IMAGE_DONE` event will carry.
    pub fn handle(&self) -> i32 {
        self.handle
    }

    /// Whether an event belongs to this decode.
    pub fn owns(&self, event_handle: i32) -> bool {
        self.handle != 0 && self.handle == event_handle
    }

    /// Collect the result. Call after the event reported `SHIM_OK` for this handle.
    pub fn take(&mut self) -> Result<Image> {
        self.images.result(self.handle)
    }

    /// What state the decode is in, for a decode that is taking longer than it should.
    pub fn progress(&mut self) -> Option<Progress> {
        self.images.describe(self.handle)
    }
}

impl<I: Images> Drop for Decoder<I> {
    fn drop(&mut self) {
        if self.handle != 0 {
            self.images.close(self.handle);
            self.handle = 0;
        }
    }
}

// ------------------------------------------------------------------ resampling --

/// Shrink or stretch to exactly `dst_w` × `dst_h`, nearest-neighbour, integer only.
///
/// This is the half the codec cannot do: the ICL reduces by powers of two, so a photo
/// bound to a 96-pixel thumbnail box arrives at 160 and still needs fitting. Nearest
/// neighbour rather than bilinear because the input is already close to the output
/// size — one power of two at worst — and averaging four pixels per output pixel costs
/// four times the memory traffic to fix banding nobody can see at this scale.
///
/// The step is a 16.16 fixed-point ratio. Floating point would be one line shorter and
/// is the wrong tool: this target is soft-float, so every multiply becomes a call.
pub fn resample(src: &Image, dst_w: i32, dst_h: i32) -> Image {
    if dst_w <= 0 || dst_h <= 0 || src.width <= 0 || src.height <= 0 {
        return Image { pixels: Vec::new(), width: 0, height: 0 };
    }
    if dst_w == src.width && dst_h == src.height {
        return src.clone();
    }

    let mut out = vec![0u16; (dst_w as usize) * (dst_h as usize)];
    let x_step = ((src.width as u32) << 16) / dst_w as u32;
    let y_step = ((src.height as u32) << 16) / dst_h as u32;

    for dy in 0..dst_h {
        let sy = (((dy as u32) * y_step) >> 16) as i32;
        let row = match src.row(sy.min(src.height - 1)) {
            Some(r) => r,
            None => continue,
        };
        let dst_row = &mut out[(dy as usize) * (dst_w as usize)..][..dst_w as usize];
        for (dx, px) in dst_row.iter_mut().enumerate() {
            let sx = (((dx as u32) * x_step) >> 16) as usize;
            *px = row[sx.min(row.len() - 1)];
        }
    }
    Image { pixels: out, width: dst_w, height: dst_h }
}

/// The largest size that fits inside `max_w` × `max_h` with the aspect ratio of
/// `w` × `h`, never enlarging.
///
/// Integer arithmetic on purpose. The version this replaces used `f32`, which on a
/// soft-float ARMv5 means a call into `compiler_builtins` for every operation — and it
/// is the same answer.
pub fn fit(w: i32, h: i32, max_w: i32, max_h: i32) -> (i32, i32) {
    if w <= 0 || h <= 0 || max_w <= 0 || max_h <= 0 {
        return (0, 0);
    }
    if w <= max_w && h <= max_h {
        return (w, h);
    }
    // Compare w/h against max_w/max_h by cross-multiplying, so the wider ratio decides
    // which edge binds. i64 because 320 * 4096 still fits i32 but a stray large photo
    // multiplied by a screen dimension need not.
    let by_width = (w as i64) * (max_h as i64) >= (h as i64) * (max_w as i64);
    if by_width {
        let out_h = ((h as i64) * (max_w as i64) / (w as i64)).max(1) as i32;
        (max_w, out_h)
    } else {
        let out_w = ((w as i64) * (max_h as i64) / (h as i64)).max(1) as i32;
        (out_w, max_h)
    }
}

// --------------------------------------------------------------------- testing --

/// An [`Images`] that decodes nothing and returns what it was given.
///
/// Completion is immediate — `start_*` hands back a handle whose result is already
/// waiting — because the ordering this fake exists to test is the caller's: does it
/// close the handle, does it match the event to the right message, does it resample to
/// the right box. Whether the codec is asynchronous is not something a host test can
/// or should reproduce.
#[derive(Debug, Default)]
pub struct MemImages {
    /// Handed out in order, one per `start_*` call.
    pub queued: Vec<Image>,
    /// Handles started and not yet closed. A test asserting this is empty is asserting
    /// the caller does not leak decoder slots.
    pub open: Vec<i32>,
    next: i32,
    /// What `start_*` should fail with, if anything. For the sticker path, which must
    /// cope with a format the device has no plugin for.
    pub fail_with: Option<Error>,
    ready: Vec<(i32, Image)>,
}

impl MemImages {
    pub fn new(queued: Vec<Image>) -> Self {
        Self { queued, ..Default::default() }
    }

    /// One solid-colour image, for tests that care about size rather than content.
    pub fn solid(w: i32, h: i32, color: u16) -> Image {
        Image { pixels: vec![color; (w * h) as usize], width: w, height: h }
    }

    fn begin(&mut self, max_w: i32, max_h: i32) -> Result<i32> {
        if let Some(e) = self.fail_with {
            return Err(e);
        }
        if self.queued.is_empty() {
            return Err(Error::NotFound);
        }
        let img = self.queued.remove(0);
        // Mimic the device: reduce only by powers of two, so a caller that assumes it
        // got exactly the box it asked for fails here rather than on the phone.
        let mut w = img.width;
        let mut h = img.height;
        while (max_w > 0 && w > max_w) || (max_h > 0 && h > max_h) {
            if w / 2 < 1 || h / 2 < 1 {
                break;
            }
            w /= 2;
            h /= 2;
        }
        let reduced = resample(&img, w, h);
        self.next += 1;
        let handle = self.next;
        self.open.push(handle);
        self.ready.push((handle, reduced));
        Ok(handle)
    }
}

impl Images for MemImages {
    fn probe(&mut self, _path: &Utf16Path) -> Result<(i32, i32)> {
        match self.queued.first() {
            Some(i) => Ok((i.width, i.height)),
            None => Err(Error::NotFound),
        }
    }

    fn start_file(&mut self, _path: &Utf16Path, max_w: i32, max_h: i32) -> Result<i32> {
        self.begin(max_w, max_h)
    }

    fn start_mem(&mut self, _data: &[u8], max_w: i32, max_h: i32) -> Result<i32> {
        self.begin(max_w, max_h)
    }

    fn result(&mut self, handle: i32) -> Result<Image> {
        match self.ready.iter().find(|(h, _)| *h == handle) {
            Some((_, img)) => Ok(img.clone()),
            None => Err(Error::Platform(sys::SHIM_ERR_BAD_HANDLE)),
        }
    }

    fn close(&mut self, handle: i32) {
        self.open.retain(|h| *h != handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_shrinks_by_the_binding_edge_and_keeps_the_ratio() {
        // Wide: width binds.
        assert_eq!(fit(1600, 1200, 320, 240), (320, 240));
        assert_eq!(fit(1000, 250, 320, 240), (320, 80));
        // Tall: height binds.
        assert_eq!(fit(250, 1000, 320, 240), (60, 240));
        // Already inside: untouched, never enlarged. A 40x40 avatar blown up to fill
        // the box would be blurry for no reason.
        assert_eq!(fit(100, 100, 320, 240), (100, 100));
        assert_eq!(fit(320, 240, 320, 240), (320, 240));
    }

    #[test]
    fn fit_refuses_nonsense_instead_of_dividing_by_zero() {
        assert_eq!(fit(0, 10, 320, 240), (0, 0));
        assert_eq!(fit(10, 0, 320, 240), (0, 0));
        assert_eq!(fit(10, 10, 0, 240), (0, 0));
        assert_eq!(fit(-5, 10, 320, 240), (0, 0));
    }

    #[test]
    fn fit_never_rounds_a_dimension_to_zero() {
        // A 4000x3 panorama into a 96-pixel box: the honest ratio is 0.07 pixels tall.
        // One is the smallest thing that can be drawn, and zero would make an empty
        // image that reads as a decode failure.
        let (w, h) = fit(4000, 3, 96, 96);
        assert_eq!(w, 96);
        assert_eq!(h, 1);
    }

    #[test]
    fn resample_halving_picks_every_other_pixel() {
        // 4x2 of distinct values, so a shear or an off-by-one is visible.
        let src = Image {
            pixels: vec![1, 2, 3, 4, 5, 6, 7, 8],
            width: 4,
            height: 2,
        };
        let out = resample(&src, 2, 1);
        assert_eq!(out.width, 2);
        assert_eq!(out.height, 1);
        assert_eq!(out.pixels, vec![1, 3]);
    }

    #[test]
    fn resample_to_the_same_size_is_the_same_image() {
        let src = MemImages::solid(3, 3, 0xF800);
        assert_eq!(resample(&src, 3, 3), src);
    }

    #[test]
    fn resample_enlarging_repeats_rather_than_reading_out_of_bounds() {
        let src = Image { pixels: vec![1, 2], width: 2, height: 1 };
        let out = resample(&src, 4, 2);
        assert_eq!(out.width, 4);
        assert_eq!(out.height, 2);
        assert_eq!(out.pixels, vec![1, 1, 2, 2, 1, 1, 2, 2]);
    }

    #[test]
    fn row_refuses_out_of_range_instead_of_panicking() {
        let img = MemImages::solid(2, 2, 7);
        assert_eq!(img.row(0), Some(&[7u16, 7][..]));
        assert_eq!(img.row(1), Some(&[7u16, 7][..]));
        assert_eq!(img.row(2), None);
        assert_eq!(img.row(-1), None);
    }

    #[test]
    fn row_refuses_a_header_that_disagrees_with_the_pixels() {
        // The shape a truncated decode would leave. Drawing code reads rows through
        // this, so it must come back None rather than slicing past the end.
        let bad = Image { pixels: vec![1, 2], width: 4, height: 4 };
        assert_eq!(bad.row(0), None);
        assert_eq!(bad.row(3), None);
    }

    /// An [`Images`] that shares its close log with the test, so the test can outlive
    /// the [`Decoder`] that owns it and still see what happened.
    #[derive(Clone)]
    struct Spy {
        closed: alloc::rc::Rc<core::cell::RefCell<Vec<i32>>>,
        inner: alloc::rc::Rc<core::cell::RefCell<MemImages>>,
    }

    impl Images for Spy {
        fn probe(&mut self, p: &Utf16Path) -> Result<(i32, i32)> {
            self.inner.borrow_mut().probe(p)
        }
        fn start_file(&mut self, p: &Utf16Path, w: i32, h: i32) -> Result<i32> {
            self.inner.borrow_mut().start_file(p, w, h)
        }
        fn start_mem(&mut self, d: &[u8], w: i32, h: i32) -> Result<i32> {
            self.inner.borrow_mut().start_mem(d, w, h)
        }
        fn result(&mut self, h: i32) -> Result<Image> {
            self.inner.borrow_mut().result(h)
        }
        fn close(&mut self, h: i32) {
            self.closed.borrow_mut().push(h);
            self.inner.borrow_mut().close(h);
        }
    }

    #[test]
    fn a_decoder_closes_its_handle_when_dropped() {
        // The device has four decode slots, each holding a CFbsBitmap in a heap the
        // whole phone shares. A caller that opens a photo, backs out, and opens
        // another must not need to remember to close: leaking four wedges the fifth.
        let spy = Spy {
            closed: alloc::rc::Rc::new(core::cell::RefCell::new(Vec::new())),
            inner: alloc::rc::Rc::new(core::cell::RefCell::new(MemImages::new(vec![
                MemImages::solid(8, 8, 0x1234),
            ]))),
        };
        let handle;
        {
            let mut d = Decoder::memory(spy.clone(), vec![0u8; 4], 8, 8).unwrap();
            handle = d.handle();
            assert!(d.owns(handle));
            assert!(!d.owns(handle + 1));
            let img = d.take().unwrap();
            assert_eq!((img.width, img.height), (8, 8));
            assert!(spy.closed.borrow().is_empty(), "not closed while still in use");
        }
        assert_eq!(*spy.closed.borrow(), vec![handle]);
        assert!(spy.inner.borrow().open.is_empty(), "the slot went back");
    }

    #[test]
    fn an_abandoned_decode_releases_its_slot() {
        // Four slots on the device; a caller that scrolls past ten photos without
        // closing them would wedge the fifth.
        let mut images = MemImages::new(vec![
            MemImages::solid(4, 4, 1),
            MemImages::solid(4, 4, 2),
        ]);
        let h1 = images.start_mem(&[0], 4, 4).unwrap();
        let h2 = images.start_mem(&[0], 4, 4).unwrap();
        assert_eq!(images.open, vec![h1, h2]);
        images.close(h1);
        assert_eq!(images.open, vec![h2]);
        images.close(h2);
        assert!(images.open.is_empty());
    }

    #[test]
    fn the_fake_reduces_by_powers_of_two_like_the_device_does() {
        // 640x480 into a 320x240 box halves exactly. Into a 200x200 box it halves
        // twice, to 160x120 — smaller than asked, which is the behaviour callers must
        // handle and the reason `resample` exists.
        let mut images = MemImages::new(vec![
            MemImages::solid(640, 480, 9),
            MemImages::solid(640, 480, 9),
        ]);
        let h = images.start_mem(&[0], 320, 240).unwrap();
        let got = images.result(h).unwrap();
        assert_eq!((got.width, got.height), (320, 240));

        let h = images.start_mem(&[0], 200, 200).unwrap();
        let got = images.result(h).unwrap();
        assert_eq!((got.width, got.height), (160, 120));
    }

    #[test]
    fn a_format_the_device_cannot_decode_is_an_error_not_a_panic() {
        // What a WebP sticker does. The caller has to fall back to drawing the emoji,
        // so the failure must arrive as a Result it can branch on.
        let unsupported = Error::Platform(sys::SHIM_ERR_NOT_SUPPORTED);
        let mut images = MemImages::new(vec![MemImages::solid(4, 4, 1)]);
        images.fail_with = Some(unsupported);
        assert_eq!(images.start_mem(&[0], 4, 4), Err(unsupported));
        assert!(unsupported.is_unsupported());
        assert!(!Error::NotFound.is_unsupported(), "absent is not the same as unsupported");
    }

    #[test]
    fn a_stale_handle_does_not_resolve() {
        let mut images = MemImages::new(vec![MemImages::solid(4, 4, 1)]);
        let h = images.start_mem(&[0], 4, 4).unwrap();
        assert!(images.result(h).is_ok());
        assert_eq!(
            images.result(h + 999),
            Err(Error::Platform(sys::SHIM_ERR_BAD_HANDLE))
        );
    }
}
