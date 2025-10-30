/*
 * Copyright 2025 Oxide Computer Company
 */

use std::{ffi::CStr, mem::size_of, time::Instant};

use anyhow::{bail, Result};
use image::RgbImage;
use libc::{c_int, c_void, memcpy};

extern "C" {
    fn arc4random_uniform(upper_bound: u32) -> u32;
}

pub struct Framebuffer {
    fb: illumos_fb::Framebuffer,
    shadow: Vec<u32>,
    clear: bool,
}

impl Framebuffer {
    pub fn new() -> Result<Framebuffer> {
        let fb = illumos_fb::Framebuffer::open_default()?;

        /*
         * Put the framebuffer in graphics mode.  This tells the kernel to stop
         * using the framebuffer device for the text terminal.
         */
        fb.mode_set(illumos_fb::Mode::Graphics)?;

        println!("real: width {} by height {}", fb.width(), fb.height());

        Ok(Framebuffer {
            shadow: vec![0u32; fb.map_len() / size_of::<u32>()],
            fb,
            /*
             * Make sure we draw every pixel at least once:
             */
            clear: true,
        })
    }

    pub fn apply(&mut self, img: &RgbImage) {
        /*
         * Drawing the whole 1280x1024 pixels (or more!) this way is somewhat
         * slow.  On the Wyse 3040 in the office (which has an enormous
         * 5120x1440 display) it is possible to visually see that the top and
         * bottom clock are updating at a slightly different time.  Split the
         * framebuffer into stripes, so that we can draw only the portions of
         * the display that are dirty.
         */
        const CHUNKS: usize = 1024;
        let mut buckets = [false; CHUNKS];
        let chsz = (self.width() * self.height()) as usize / CHUNKS;

        let pixl = self.fb.pixel_layout();

        for (idx, px) in img.as_raw().chunks(3).enumerate() {
            let pix = pixl.rgb_to_pix(px[0], px[1], px[2]);
            if self.shadow[idx] != pix {
                self.shadow[idx] = pix;
                buckets[idx / chsz] = true;
            }
        }

        /*
         * First, filter out only the buckets we need to draw:
         */
        let mut indexes = buckets
            .into_iter()
            .enumerate()
            .filter(|(_, dirty)| self.clear || *dirty)
            .map(|(idx, _)| idx)
            .collect::<Vec<_>>();

        /*
         * Copy chunks from the shadow buffer to the mapped framebuffer.  Rust
         * continues to drag its feet on having a proper volatile memcpy, but
         * we've got memcpy(3C) at home.
         */
        let targ = unsafe { self.fb.map() };
        let buf = self.shadow.as_ptr() as *const u32;
        for idx in indexes {
            let offs = idx * chsz;

            unsafe {
                libc::memcpy(
                    targ.add(offs) as *mut c_void,
                    buf.add(offs) as *const c_void,
                    chsz * size_of::<u32>(),
                )
            };
        }

        self.clear = false;
    }

    pub fn height(&self) -> u32 {
        self.fb.height().try_into().unwrap()
    }

    pub fn width(&self) -> u32 {
        self.fb.width().try_into().unwrap()
    }
}

impl Drop for Framebuffer {
    fn drop(&mut self) {
        /*
         * Try to clean up a bit by clearing the screen and re-enabling the
         * kernel text terminal.
         */
        self.fb.clear();
        self.fb.mode_set(illumos_fb::Mode::ResetText).ok();
    }
}
