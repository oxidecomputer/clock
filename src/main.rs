/*
 * Copyright 2024 Oxide Computer Company
 */

use std::{
    iter::once,
    net::{Ipv4Addr, SocketAddr},
    ops::RangeInclusive,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{bail, Result};
use chrono::prelude::*;
use image::{GenericImage, ImageBuffer, Rgb, RgbImage};
use rusttype::{point, Font, Scale};

#[cfg(target_os = "illumos")]
mod ctf;
#[cfg(target_os = "illumos")]
mod fb;
mod http;
#[cfg(target_os = "illumos")]
mod kvm;
mod utils;
#[cfg(target_os = "linux")]
mod x11;

use slog::Logger;

struct Message {
    rgb: Rgb<u8>,
    text: String,
    height: u32,
    flash: Option<Duration>,
}

struct Inner {
    msg: Option<Message>,
    image: Option<ImageBuffer<Rgb<u8>, Vec<u8>>>,
    width: u32,
    height: u32,
    countdown: Option<Countdown>,
}

struct Countdown {
    until: Instant,
}

struct App {
    log: Logger,
    inner: Mutex<Inner>,
}

trait RgbExt {
    fn attenuate(&self, v: f32) -> Self;
}

impl RgbExt for Rgb<u8> {
    fn attenuate(&self, v: f32) -> Rgb<u8> {
        Rgb([
            ((self.0[0] as f32) * v) as u8,
            ((self.0[1] as f32) * v) as u8,
            ((self.0[2] as f32) * v) as u8,
        ])
    }
}

enum Align {
    Left(u32),
    Right(u32),
    Centre(u32, u32),
}

fn horiz_line(
    x0: u32,
    x1: u32,
    y: u32,
    width: u32,
    rgb: Rgb<u8>,
    img: &mut RgbImage,
) {
    let (y0, y1) = if width < 2 {
        (y, y)
    } else {
        (y.saturating_sub(width / 2), y.saturating_add(width / 2))
    };

    for x in x0..x1 {
        if x >= img.width() {
            continue;
        }

        for y in y0..=y1 {
            if y >= img.height() {
                continue;
            }

            img.put_pixel(x, y, rgb);
        }
    }
}

fn emit_text(
    text: &str,
    xa: Align,
    y: u32,
    fonts: &FontStack,
    pxht: u32,
    rgb: Rgb<u8>,
    img: &mut RgbImage,
    fixed_numbers: bool,
    fixed_extra: bool,
) -> u32 {
    let height = pxht as f32;

    let scale = Scale::uniform(height);

    let num_width = if fixed_numbers {
        let mut max = 0f32;
        for c in ('0'..='9').chain(once(' ')).chain(once(':')) {
            let font = fonts.for_glyph(c);
            let tw = font.glyph(c).scaled(scale).h_metrics().advance_width;
            if tw > max {
                max = tw;
            }
        }
        if fixed_extra {
            for c in once('m').chain(once('s')) {
                let font = fonts.for_glyph(c);
                let tw = font.glyph(c).scaled(scale).h_metrics().advance_width;
                if tw > max {
                    max = tw;
                }
            }
        }
        Some(max)
    } else {
        None
    };

    /*
     * First, determine the width of the whole string:
     */
    let mut pgs = Vec::new();
    let mut x = 0f32;
    for c in text.chars() {
        let font = fonts.for_glyph(c);
        let v_metrics = font.v_metrics(scale);

        let g = font.glyph(c).scaled(scale);
        let (xo, w) =
            if fixed_numbers && (c.is_ascii_digit() || c == ' ' || c == ':') {
                let fw = num_width.unwrap();
                ((fw - g.h_metrics().advance_width) / 2.0, fw)
            } else {
                let fw = g.h_metrics().advance_width;
                (0.0, fw)
            };

        let g = g.positioned(point(x + xo, y as f32 + v_metrics.ascent));
        x += w;

        pgs.push(g);
    }
    let text_width = x;

    /*
     * Now that we know how wide it will be, we know where to begin drawing:
     */
    let xbase = match xa {
        Align::Left(x) => x as f32,
        Align::Right(x) => x as f32 - text_width,
        Align::Centre(x, w) => {
            let w = w as f32;
            if text_width >= w {
                /*
                 * We are too wide for the region as specified.  Just start on
                 * the left.
                 */
                x as f32
            } else {
                (x as f32) + (w - text_width) / 2.0
            }
        }
    };

    for g in pgs {
        if let Some(bb) = g.pixel_bounding_box() {
            g.draw(|x, y, v| {
                let x = (xbase as u32 + x) as i32 + bb.min.x;
                let y = y as i32 + bb.min.y;

                let x = x as u32;
                let y = y as u32;

                if x < img.width() && y < img.height() {
                    img.put_pixel(x, y, rgb.attenuate(v));
                }
            });
        }
    }

    text_width as u32
}

fn load_font(
    data: &[u8],
    glyph_ranges: Vec<RangeInclusive<u32>>,
) -> Result<FontStackEntry> {
    let Some(font) = Font::try_from_bytes(data) else {
        bail!("could not load font");
    };
    Ok(FontStackEntry { font, glyph_ranges })
}

struct FontStackEntry<'a> {
    font: Font<'a>,
    glyph_ranges: Vec<RangeInclusive<u32>>,
}

struct FontStack<'a> {
    entries: Vec<FontStackEntry<'a>>,
}

impl FontStack<'_> {
    fn for_glyph(&self, c: char) -> &Font {
        let fse = self
            .entries
            .iter()
            .filter(|fse| {
                fse.glyph_ranges.iter().any(|r| r.contains(&(c as u32)))
            })
            .next();

        if let Some(fse) = fse {
            &fse.font
        } else {
            &self.entries[self.entries.len() - 1].font
        }
    }
}

trait DateTimeExt {
    fn home(&self) -> DateTime<Local>;
}

impl DateTimeExt for DateTime<Utc> {
    fn home(&self) -> DateTime<Local> {
        DateTime::<Local>::from(*self)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let app = Arc::new(App {
        log: utils::make_log("corner"),
        inner: Mutex::new(Inner {
            msg: None,
            image: None,
            countdown: None,
            height: 1,
            width: 1,
        }),
    });

    let app0 = app.clone();
    tokio::task::spawn(async {
        http::server(app0, SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 8888))
            .await
            .unwrap();
    });

    #[cfg(target_os = "linux")]
    /*
     * The target display in the office is 5120 x 1440, but obviously that's
     * tremendously large.  For development convenience, create a much smaller
     * window, but which has the expected aspect ratio:
     */
    let mut fb = x11::App::open(5120 / 4, 1440 / 4)?;

    #[cfg(target_os = "illumos")]
    let mut fb = fb::Framebuffer::new()?;

    let fonts = FontStack {
        entries: vec![
            load_font(
                include_bytes!("../fonts/unifont-15.0.01.ttf"),
                vec![
                    /*
                     * Basic icons in this range (sun and moon):
                     */
                    0x2600..=0x26ff,
                    /*
                     * Dingbats (e.g., aeroplane):
                     */
                    0x2700..=0x27bf,
                ],
            )?,
            load_font(
                include_bytes!("../fonts/unifont_upper-15.0.01.ttf"),
                vec![
                    /*
                     * Birthday Cake, Jack-o-lantern, Christmas Tree:
                     */
                    0x1F382..=0x1F384,
                    /*
                     * Bottle with popping cork:
                     */
                    0x1F37E..=0x1F37E,
                ],
            )?,
            load_font(
                include_bytes!("../fonts/Domine-Regular.ttf"),
                vec![
                    /*
                     * Everything else:
                     */
                    1..=0x25FF,
                ],
            )?,
        ],
    };

    #[cfg(target_os = "linux")]
    let mut img = RgbImage::new(fb.width(), fb.height());

    #[cfg(target_os = "illumos")]
    let mut img = RgbImage::new(
        fb.width().try_into().unwrap(),
        fb.height().try_into().unwrap(),
    );

    {
        let mut i = app.inner.lock().unwrap();
        i.height = img.height();
        i.width = img.width();
    }

    let clocks = [("Oxide", chrono_tz::US::Pacific)];

    let ch = img.height() / clocks.len() as u32;

    #[cfg(target_os = "illumos")]
    fn paint(fb: &mut fb::Framebuffer, img: &ImageBuffer<Rgb<u8>, Vec<u8>>) {
        fb.apply(img);
    }

    #[cfg(target_os = "linux")]
    fn paint(fb: &mut x11::App, img: &ImageBuffer<Rgb<u8>, Vec<u8>>) {
        fb.apply(img);
        fb.poll().expect("x11 poll");
    }

    loop {
        let now = Utc::now();
        let inow = Instant::now();

        img.fill(0);

        {
            let i = app.inner.lock().unwrap();

            /*
             * We've got a countdown timer to render!
             */
            if let Some(cd) = i.countdown.as_ref() {
                fn durstr(dur: Duration) -> String {
                    let mut secs = dur.as_secs();

                    let mins = secs / 60;
                    secs -= mins * 60;

                    if mins == 0 {
                        format!("{secs:2} s")
                    } else {
                        format!("{mins:2} m {secs:2} s")
                    }
                }

                /*
                 * How much time remains until the countdown timer expires?
                 */
                let (colour, msg, msecoff) = if let Some(rem) =
                    cd.until.checked_duration_since(inow)
                {
                    let mut x = rem.as_millis() as u64;
                    while x > 1000 {
                        x -= 1000;
                    }

                    (Rgb([0x48, 0xd5, 0x97]), durstr(rem), x)
                } else {
                    /*
                     * The timer has expired.  How long has it been?
                     */
                    if let Some(ela) = inow.checked_duration_since(cd.until) {
                        let mut x = ela.as_millis() as u64;
                        while x > 1000 {
                            x -= 1000;
                        }
                        x = 1000 - x;

                        (Rgb([0xff, 0, 0]), durstr(ela), x)
                    } else {
                        /*
                         * We are very confused!
                         */
                        (Rgb([0xff, 0, 0]), "timer expired!".into(), 1000)
                    }
                };

                let ch = img.height() as u32;
                let ht = ch * 11 / 18;
                emit_text(
                    &msg,
                    Align::Centre(0, img.width()),
                    (ch - ht - (ht / 3)) / 2,
                    &fonts,
                    ht,
                    colour,
                    &mut img,
                    true,
                    false,
                );

                paint(&mut fb, &img);

                std::thread::sleep(Duration::from_millis(25));
                //std::thread::sleep(Duration::from_millis(
                //    msecoff.saturating_sub(200),
                //));
                continue;
            }

            /*
             * We've been given a picture to display via the HTTP API.  Draw
             * that on the screen:
             */
            if let Some(over) = i.image.as_ref() {
                /*
                 * Screen ratio:
                 */
                let irat = img.width() as f32 / img.height() as f32;

                /*
                 * Image ratio:
                 */
                let orat = over.width() as f32 / over.height() as f32;

                let (w, h) = if irat > orat {
                    /*
                     * The display is wider than the picture.
                     */
                    ((img.height() as f32 * orat) as u32, img.height())
                } else {
                    /*
                     * The picture is wider than the display.
                     */
                    (img.width(), (img.width() as f32 / orat) as u32)
                };

                let x = (img.width() - w) / 2;
                let y = (img.height() - h) / 2;

                img.copy_from(over, x, y).ok();

                paint(&mut fb, &img);

                std::thread::sleep(Duration::from_secs(1));
                continue;
            }

            /*
             * We've been given a message (text) to display on the screen via
             * the HTTP API.  Draw that on the screen:
             */
            if let Some(m) = i.msg.as_ref() {
                emit_text(
                    &m.text,
                    Align::Centre(0, img.width()),
                    (img.height() - m.height) / 2,
                    &fonts,
                    m.height,
                    m.rgb,
                    &mut img,
                    false,
                    false,
                );

                paint(&mut fb, &img);

                if let Some(flash) = m.flash {
                    std::thread::sleep(flash);

                    img.fill(0);
                    paint(&mut fb, &img);

                    std::thread::sleep(flash);
                } else {
                    /*
                     * When not actually rendering the time, and not flashing,
                     * just sleep for a second.
                     */
                    std::thread::sleep(Duration::from_secs(1));
                }

                continue;
            }
        }

        /*
         * If neither an image nor a message have been furnished for display,
         * render the current time and date.
         */
        for (idx, (_name, tz)) in clocks.iter().enumerate() {
            let now = now.with_timezone(tz);
            let yc = ch * idx as u32;

            if idx > 0 {
                horiz_line(
                    0,
                    img.width(),
                    yc,
                    4,
                    Rgb([0xc8, 0xc8, 0xc8]),
                    &mut img,
                )
            }

            let ht = ch / 4;

            let grey = Rgb([0x7d, 0x83, 0x85]);

            emit_text(
                &now.format("%d %B %Y").to_string(),
                Align::Right(img.width() - 1),
                yc + ch - ht - 10,
                &fonts,
                ht,
                grey,
                &mut img,
                false,
                false,
            );

            emit_text(
                &now.format("%A").to_string(),
                Align::Left(0),
                yc + ch - ht - 10,
                &fonts,
                ht,
                grey,
                &mut img,
                false,
                false,
            );

            /*
             * Approximately oxide green:
             */
            let colour = Rgb([0x48, 0xd5, 0x97]);

            let ht = ch * 10 / 18;
            emit_text(
                &now.format("%H:%M:%S").to_string(),
                Align::Centre(0, img.width()),
                yc + (ch - ht - (ht / 3)) / 2,
                &fonts,
                ht,
                colour,
                &mut img,
                true,
                false,
            );
        }

        paint(&mut fb, &img);

        std::thread::sleep(
            /*
             * Wind our original hrtime measurement back to the start of the
             * second we are rendering...
             */
            inow.checked_sub(Duration::from_nanos(
                now.timestamp_subsec_nanos() as u64,
            ))
            .unwrap()
            /*
             * Then wind it forward by one whole second, plus a fudge factor to
             * ensure we end up in the target second...
             */
            .checked_add(Duration::from_millis(1000 + 10))
            .unwrap()
            /*
             * Then sleep for the time that remains between now and that
             * projected time:
             */
            .saturating_duration_since(Instant::now()),
        );
    }
}
