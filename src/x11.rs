/*
 * Copyright 2024 Oxide Computer Company
 */

use anyhow::{bail, Result};
use image::RgbImage;
use x11rb::atom_manager;
use x11rb::connection::Connection;
use x11rb::image::Image;
use x11rb::properties::WmSizeHints;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

atom_manager! {
    pub Atoms: AtomsCookie {
        _NET_WM_NAME,
        UTF8_STRING,
        WM_DELETE_WINDOW,
        WM_PROTOCOLS,
    }
}

#[allow(unused)]
pub struct App<'a> {
    atoms: Atoms,
    win: Window,
    w: u16,
    h: u16,
    conn: RustConnection,
    screen_num: usize,

    black: Gcontext,

    pix: Pixmap,
    buf: Image<'a>,

    keys: GetKeyboardMappingReply,
}

impl<'a> App<'a> {
    pub fn open<'b>(scrw: u16, scrh: u16) -> Result<App<'b>> {
        let (conn, screen_num) = x11rb::connect(None)?;
        let atoms = Atoms::new(&conn)?.reply()?;

        let screen = &conn.setup().roots[screen_num];
        if screen.root_depth != 24 {
            bail!("only works with 24-bit true colour displays");
        }

        let keys = conn
            .get_keyboard_mapping(
                conn.setup().min_keycode,
                conn.setup().max_keycode - conn.setup().min_keycode,
            )?
            .reply()?;

        let win = conn.generate_id()?;
        let aux = CreateWindowAux::new().event_mask(
            EventMask::EXPOSURE
                | EventMask::STRUCTURE_NOTIFY
                | EventMask::KEY_RELEASE,
        );

        let black = conn.generate_id()?;
        conn.create_gc(
            black,
            screen.root,
            &CreateGCAux::new()
                .graphics_exposures(0)
                .foreground(screen.black_pixel),
        )?;

        /*
         * Create our drawing window.
         */
        conn.create_window(
            screen.root_depth,
            win,
            screen.root,
            0,
            0,
            scrw,
            scrh,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &aux,
        )?;

        let buf = x11rb::image::Image::allocate_native(
            scrw,
            scrh,
            screen.root_depth,
            conn.setup(),
        )?;

        /*
         * Create the pixmap we are going to draw into prior to copying to the
         * target window.
         */
        let pix = conn.generate_id()?;
        conn.create_pixmap(screen.root_depth, pix, win, scrw, scrh)?;

        buf.put(&conn, pix, black, 0, 0)?;

        let title = "bsfb";
        conn.change_property8(
            PropMode::REPLACE,
            win,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            title.as_bytes(),
        )?;
        conn.change_property8(
            PropMode::REPLACE,
            win,
            atoms._NET_WM_NAME,
            atoms.UTF8_STRING,
            title.as_bytes(),
        )?;

        conn.change_property32(
            PropMode::REPLACE,
            win,
            atoms.WM_PROTOCOLS,
            AtomEnum::ATOM,
            &[atoms.WM_DELETE_WINDOW],
        )?;

        let mut wsh = WmSizeHints::new();
        wsh.min_size = Some((scrw as i32, scrh as i32));
        wsh.max_size = Some((scrw as i32, scrh as i32));
        wsh.base_size = Some((scrw as i32, scrh as i32));

        wsh.set_normal_hints(&conn, win)?;

        conn.map_window(win)?;
        conn.flush()?;

        Ok(App {
            atoms,
            win,
            w: scrw,
            h: scrh,
            conn,
            screen_num,
            pix,
            buf,
            black,
            keys,
        })
    }

    fn redraw(&mut self) -> Result<()> {
        self.buf.put(&self.conn, self.pix, self.black, 0, 0)?;
        self.flip()?;

        Ok(())
    }

    fn flip(&self) -> Result<()> {
        /*
         * The backing pixmap always contains the current rendered screen, so we
         * can just copy it to the window.
         */
        self.conn.copy_area(
            self.pix,
            self.win,
            self.black,
            0,
            0,
            0,
            0,
            self.w.min(self.buf.width()),
            self.h.min(self.buf.height()),
        )?;

        /*
         * The backing pixmap may not actually cover the entire window.  Overlay
         * the missing parts with black rectangles.
         */
        if self.w > self.buf.width() {
            self.conn.poly_fill_rectangle(
                self.win,
                self.black,
                &[Rectangle {
                    x: self.buf.width().try_into().unwrap(),
                    y: 0,
                    width: self.w - self.buf.width(),
                    height: self.h,
                }],
            )?;
        }
        if self.h > self.buf.height() {
            self.conn.poly_fill_rectangle(
                self.win,
                self.black,
                &[Rectangle {
                    x: 0,
                    y: self.buf.height().try_into().unwrap(),
                    width: self.w,
                    height: self.h - self.buf.height(),
                }],
            )?;
        }

        self.conn.flush()?;
        Ok(())
    }

    pub fn width(&self) -> u32 {
        self.w.try_into().unwrap()
    }

    pub fn height(&self) -> u32 {
        self.h.try_into().unwrap()
    }

    pub fn apply(&mut self, img: &RgbImage) {
        for x in 0..img.width().min(self.buf.width() as u32) {
            for y in 0..img.height().min(self.buf.height() as u32) {
                let px = img.get_pixel(x, y);
                let rgb = (px[2] as u32) << 0
                    | (px[1] as u32) << 8
                    | (px[0] as u32) << 16;
                self.buf.put_pixel(x as u16, y as u16, rgb);
            }
        }

        self.redraw().expect("redraw");
    }

    pub fn poll(&mut self) -> Result<()> {
        while let Some(ev) = self.conn.poll_for_event()? {
            match ev {
                Event::MapNotify(_) | Event::ReparentNotify(_) => {}
                Event::ConfigureNotify(ev) => {
                    if ev.window != self.win {
                        println!("??? configure for wrong window: {ev:?}");
                        continue;
                    }

                    self.w = ev.width;
                    self.h = ev.height;

                    self.flip()?;
                }
                Event::Expose(ev) => {
                    if ev.window != self.win {
                        println!("??? expose for wrong window: {ev:?}");
                        continue;
                    }

                    self.flip()?;
                }
                Event::ClientMessage(ev) => {
                    let data = ev.data.as_data32();

                    if ev.format == 32
                        && ev.window == self.win
                        && data[0] == self.atoms.WM_DELETE_WINDOW
                    {
                        /*
                         * XXX terminate program
                         */
                        println!("delete window event");
                        return Ok(());
                    }

                    println!("client message: {ev:?}");
                }
                other => {
                    println!("other event: {other:?}");
                }
            }
        }

        Ok(())
    }
}
