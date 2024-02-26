/*
 * Copyright 2024 Oxide Computer Company
 */

use std::{ffi::CStr, mem::size_of, time::Instant};

use anyhow::{bail, Result};
use image::RgbImage;
use libc::{c_int, c_void};

use crate::{ctf::Ctf, kvm::Kvm};

extern "C" {
    fn arc4random_uniform(upper_bound: u32) -> u32;
}

fn check_ret_fd(fv: c_int, msg: &str) -> Result<()> {
    if fv < 0 {
        let e = std::io::Error::last_os_error();
        bail!("{msg}: {e}");
    } else {
        Ok(())
    }
}

pub struct Framebuffer {
    fd: c_int,
    width: u64,
    height: u64,
    baseaddr: u64,
    #[allow(unused)]
    size: usize,
    shadow: Vec<u8>,
    clear: bool,
    last_clear: Instant,
}

impl Framebuffer {
    pub fn new() -> Result<Framebuffer> {
        /*
         * First we need to get the type information for the framebuffer struct.
         */
        let ctf = Ctf::open("/system/object/gfx_private/object")?;
        let fb_info = ctf.lookup_struct("struct fb_info")?;
        let fb = ctf.offset_of(fb_info, "fb")?;
        let fb_size = ctf.offset_of(fb_info, "fb_size")?;
        let fb_screen = ctf.offset_of(fb_info, "screen")?;
        let fb_info_pixel_coord =
            ctf.lookup_struct("struct fb_info_pixel_coord")?;
        let fb_ipc_x = ctf.offset_of(fb_info_pixel_coord, "x")?;
        let fb_ipc_y = ctf.offset_of(fb_info_pixel_coord, "y")?;

        /*
         * Look at the live kernel to get details about the framebuffer mapping.
         */
        let kvm = Kvm::new()?;
        let addr = kvm.locate("fb_info")?;
        println!("fb_info @ {addr:x}");

        let fp = CStr::from_bytes_with_nul(b"/dev/allkmem\0").unwrap();

        let fd = unsafe { libc::open(fp.as_ptr(), libc::O_RDWR) };
        check_ret_fd(fd, "open")?;

        let mut buf: usize = 0;
        let maddr = (addr as usize) + fb;
        println!("fb @ {maddr:x}");
        let r = unsafe {
            libc::pread(
                fd,
                &mut buf as *mut usize as *mut c_void,
                size_of::<usize>(),
                maddr as i64,
            )
        };
        if r != <usize as TryInto<isize>>::try_into(size_of::<usize>()).unwrap()
        {
            bail!("could not read \"fb\"");
        }
        let baseaddr = buf;

        let maddr = (addr as usize) + fb_size;
        println!("fb_size @ {maddr:x}");
        let r = unsafe {
            libc::pread(
                fd,
                &mut buf as *mut usize as *mut c_void,
                size_of::<usize>(),
                maddr as i64,
            )
        };
        if r != <usize as TryInto<isize>>::try_into(size_of::<usize>()).unwrap()
        {
            bail!("could not read \"fb_size\"");
        }
        let size = buf;

        println!("baseaddr = {baseaddr:x}");
        println!("size = {size:x}");

        /*
         * Read the screen size.
         */
        let width = kvm.read_u16(addr + fb_screen + fb_ipc_x)?;
        let height = kvm.read_u16(addr + fb_screen + fb_ipc_y)?;

        println!("width {width} by height {height}");

        Ok(Framebuffer {
            fd,
            baseaddr: baseaddr.try_into().unwrap(),
            width: width.into(),
            height: height.into(),
            size,
            shadow: vec![0u8; size],
            clear: true,
            last_clear: Instant::now(),
        })
    }

    pub fn apply(&mut self, img: &RgbImage) {
        let this_draw = Instant::now();
        if this_draw.saturating_duration_since(self.last_clear).as_secs() > 15 {
            /*
             * Redraw the whole display every 15 seconds just in case we end up
             * competing with console fluff...
             */
            self.clear = true;
            self.last_clear = this_draw;
        }

        /*
         * Drawing the whole 1280x1024 pixels (or more!) this way is somewhat
         * slow.  On the Wyse 3040 in the office (which has an enormous
         * 5120x1440 display) it is possible to visually see that the top and
         * bottom clock are updating at a slightly different time.  Split the
         * framebuffer into stripes, so that we can draw only the portions of
         * the display that are dirty.
         */
        const CHUNKS: usize = 256;
        let mut buckets = [false; CHUNKS];
        let chsz = (self.width * self.height) as usize / CHUNKS;

        for (idx, px) in img.as_raw().chunks(3).enumerate() {
            if self.shadow[idx * 4 + 2] != px[0] {
                self.shadow[idx * 4 + 2] = px[0];
                buckets[idx / chsz] = true;
            }
            if self.shadow[idx * 4 + 1] != px[1] {
                self.shadow[idx * 4 + 1] = px[1];
                buckets[idx / chsz] = true;
            }
            if self.shadow[idx * 4 + 0] != px[2] {
                self.shadow[idx * 4 + 0] = px[2];
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
         * Because our direct framebuffer writes are slow enough to be visible,
         * there was a sort of "swoop" effect once a second in which at least
         * one digit appears to roll off the display.  This has been reported to
         * cause a sort of motion sickness in some individuals.
         *
         * If we randomise the order in which we draw buckets that require an
         * update, this swoop becomes more of a dissolve, which is hopefully
         * less visually jarring.
         *
         * Use the Fisher-Yates shuffle to randomise the draw order:
         */
        for i in 1..indexes.len() {
            let j = unsafe { arc4random_uniform((i + 1) as u32) } as usize;
            indexes.swap(i, j);
        }

        let buf = self.shadow.as_ptr() as *const c_void;
        for idx in indexes {
            let chsz = ((self.width * self.height) as usize / CHUNKS) * 4;
            let offs = (self.baseaddr as i64) + (idx as i64) * (chsz as i64);
            let buf = (buf as usize) + (idx as usize) * (chsz as usize);

            unsafe { libc::pwrite(self.fd, buf as *const c_void, chsz, offs) };
        }

        self.clear = false;
    }

    pub fn height(&self) -> usize {
        self.height.try_into().unwrap()
    }

    pub fn width(&self) -> usize {
        self.width.try_into().unwrap()
    }
}

impl Drop for Framebuffer {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}
