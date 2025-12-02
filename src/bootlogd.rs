/*
 * bootlogd.rs	Store output from the console during bootup into a file.
 *
 * Version:	@(#)bootlogd  0.1.0 12-01-2025 Palindromic Bread Loaf (herbthehaircut@proton.me)
 *
 *      This file is part of the rye-init suite, a rewrite of the sysvinit suite in rust,
 *      Copyright (C) 2025 Palindromic Bread Loaf
 *
 *		This file uses references from the sysvinit suite,
 *		Copyright (C) 1991-2004 Miquel van Smoorenburg.
 *
 *		This program is free software; you can redistribute it and/or modify
 *		it under the terms of the GNU General Public License as published by
 *		the Free Software Foundation; either version 3 of the License, or
 *		(at your option) any later version.
 *
 *		This program is distributed in the hope that it will be useful,
 *		but WITHOUT ANY WARRANTY; without even the implied warranty of
 *		MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 *		GNU General Public License for more details.
 *
 *		You should have received a copy of the GNU General Public License
 *		along with this program; if not, write to the Free Software
 *		Foundation, Inc., 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301 USA
 *
 */
use libc;
use std::io::Write;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use crate::GOT_SIGNALS;

const MAX_CONSOLES: i8 = 16;
const KERNEL_COMMAND_LENGTH: i16 = 4096;
const LOGFILE: &str = "/var/log/boot";
const PATH_MAX: i16 = 2048;
const RINGBUF_SIZE: usize = 32768;
static GOT_SIGNAL: AtomicI32 = AtomicI32::new(0);
fn set_signal(signal: i32) {
    GOT_SIGNAL.store(signal, Ordering::SeqCst);
}

fn get_signal() -> bool {
    GOT_SIGNAL.load(Ordering::SeqCst) != 0
}

struct RingBuf {
    buf: [u8; RINGBUF_SIZE],
    in_idx: usize,
    out_idx: usize,
}

impl RingBuf {
    fn new() -> Self {
        Self {
            buf: *Box::new([0u8; RINGBUF_SIZE]),
            in_idx: 0,
            out_idx: 0,
        }
    }

    // Write up to data.len bytes into the ring buffer starting at in_idx
    // Returns the number of bytes written
    fn push(&mut self, data: &[u8]) -> usize {
        let written: usize;
        let space: usize = if self.in_idx >= self.out_idx {
            RINGBUF_SIZE - self.in_idx
        } else {
            self.out_idx - self.in_idx
        };

        let to_write = std::cmp::min(data.len(), space);

        if to_write == 0 {
            return 0;
        }

        // This is not one-to-one with how the original bootlogd.c did it. In there,
        // inptr could move outptr if it crossed it. Here we don't wrap, but there may
        // be reason to implement exact behaviour in the future.

        self.buf[self.in_idx..self.in_idx + to_write].copy_from_slice(&data[..to_write]);
        self.in_idx = (self.in_idx + to_write) % RINGBUF_SIZE;
        written = to_write;

        written
    }

    // Get a continuous slice of available data starting at out_idx
    fn get_slice(&self) -> &[u8] {
        if self.out_idx <= self.in_idx {
            &self.buf[self.out_idx..self.in_idx]
        } else {
            &self.buf[self.out_idx..RINGBUF_SIZE]
        }
    }

    // Advance the outside pointer by length wrapping around at ring buffer size
    fn advance_out(&mut self, length: usize) {
        self.out_idx = (self.out_idx + length) % RINGBUF_SIZE;
    }

    fn available(&self) -> usize {
        if self.in_idx >= self.out_idx {
            self.in_idx - self.out_idx
        } else {
            RINGBUF_SIZE - self.in_idx + self.out_idx
        }
    }
}

fn write_log(
    fp: &mut File,
    data: &[u8],
    syncalot: bool,
    print_escape_characters: bool,
    first_run: &mut bool,
) -> io::Result<()> {
    let mut inside_esc: u8 = 0;
    let mut should_sync = false;

    let mut i = 0;
    while i < data.len() {
        if (*first_run) {
            let now = SystemTime::now();
            if let Ok(n) = now.duration_since(UNIX_EPOCH) {
                let ts = format!("{:?}", now);
                write!(fp, "{}: ", ts)?;
            } else {
                write!(fp, "?: ")?;
            }
            should_sync = true;
            *first_run = false;
        }

        let byte = data[i];
        let mut ignore = false;

        if !print_escape_characters {
            if inside_esc == 1 {
                if byte == b'[' {
                    ignore = true;
                    inside_esc = 2;
                } else {
                    if byte >= 64 && byte <= 95 {
                        ignore = true;
                    }
                    inside_esc = 0;
                }
            } else if inside_esc == 2 {
                match byte {
                    b'0'..=b'9' | b';' | 32..=47 => {
                        if inside_esc != 0 {
                            ignore = true;
                        }
                    }
                    64..=126 => {
                        if inside_esc != 0 {
                            ignore = true;
                            inside_esc = 0;
                        }
                    }
                    _ => {}
                }
            } else {
                match byte {
                    b'\r' => {
                        ignore = true;
                    }
                    27 => {
                        // ESC
                        ignore = true;
                        inside_esc = 1;
                    }
                    _ => {}
                }
            }
        }

        if !ignore {
            fp.write_all(&[byte])?;
        }

        // if the byte written was newline, next char should get timestamp prefix.
        // Approximate this by checking last byte, hopefully this is okay?
        if byte == b'\n' {
            *first_run = true;
        }

        i += 1;
    }

    if should_sync {
        fp.flush()?;
        if syncalot {
            unsafe {
                libc::fsync(fp.as_raw_fd());
            }
        }
    }

    Ok(())
}
