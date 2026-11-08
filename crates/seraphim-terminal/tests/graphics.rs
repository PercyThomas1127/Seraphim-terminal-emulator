//! Kitty graphics protocol and Sixel through the public `Terminal` API.

use seraphim_terminal::Terminal;

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn apc(control: &str, payload: &[u8]) -> Vec<u8> {
    format!("\x1b_G{control};{}\x1b\\", b64(payload)).into_bytes()
}

fn term() -> Terminal {
    let mut t = Terminal::with_scrollback(20, 6, 100);
    t.set_cell_pixel_size(10.0, 20.0);
    t
}

fn reply(t: &mut Terminal) -> String {
    String::from_utf8(t.take_responses()).unwrap()
}

fn rgba(w: usize, h: usize, px: [u8; 4]) -> Vec<u8> {
    px.repeat(w * h)
}

#[test]
fn transmit_and_display_rgba_moves_the_cursor() {
    let mut t = term();
    t.advance(b"ab");
    t.advance(&apc("a=T,f=32,s=25,v=30,i=7", &rgba(25, 30, [255, 0, 0, 255])));
    assert_eq!(reply(&mut t), "\x1b_Gi=7;OK\x1b\\");
    let v = t.visible_images();
    assert_eq!(v.len(), 1);
    let p = v[0].placement;
    assert_eq!((v[0].row, p.col, p.cols, p.rows), (0, 2, 3, 2), "25x30 px in 10x20 cells");
    assert_eq!((v[0].image.width, v[0].image.height), (25, 30));
    let c = t.cursor();
    assert_eq!((c.row, c.col), (1, 5), "last image row, column after it");
    // Text keeps flowing after the image.
    t.advance(b"x");
    assert_eq!(t.screen().row(1).cells[5].ch, 'x');
}

#[test]
fn chunked_zlib_png_and_quiet() {
    let mut t = term();
    // RGB, zlib-compressed, sent in three chunks; quiet = no OK reply.
    let raw = rgba(4, 4, [0, 255, 0, 255])
        .chunks(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect::<Vec<u8>>();
    let z = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
    let enc = b64(&z);
    let chunks: Vec<&str> =
        enc.as_bytes().chunks(8).map(|c| std::str::from_utf8(c).unwrap()).collect();
    let n = chunks.len();
    for (i, c) in chunks.iter().enumerate() {
        let more = if i + 1 < n { 1 } else { 0 };
        let ctl = if i == 0 {
            format!("a=t,f=24,s=4,v=4,o=z,i=3,q=1,m={more}")
        } else {
            format!("m={more}")
        };
        t.advance(format!("\x1b_G{ctl};{c}\x1b\\").as_bytes());
    }
    assert_eq!(reply(&mut t), "", "q=1 suppresses OK");
    let img = t.graphics().image(3).expect("stored");
    assert_eq!(&img.rgba[..4], &[0, 255, 0, 255]);
    assert!(t.visible_images().is_empty(), "a=t only stores");

    // PNG via put by image number.
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, 2, 1);
        enc.set_color(png::ColorType::Rgba);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&[1, 2, 3, 255, 4, 5, 6, 128]).unwrap();
    }
    t.advance(&apc("a=t,f=100,I=42", &png));
    let r = reply(&mut t);
    assert!(r.starts_with("\x1b_Gi=") && r.contains(",I=42;OK"), "{r:?}");
    t.advance(b"\x1b_Ga=p,I=42,c=4,r=1,C=1\x1b\\");
    let v = t.visible_images();
    assert_eq!(v[0].image.rgba[4..8], [4, 5, 6, 128]);
    assert!(v[0].placement.fill && v[0].placement.cols == 4);
    assert_eq!(t.cursor().col, 0, "C=1 keeps the cursor");
}

#[test]
fn errors_are_reported_with_the_id() {
    let mut t = term();
    t.advance(b"\x1b_Ga=p,i=99\x1b\\");
    assert!(reply(&mut t).starts_with("\x1b_Gi=99;ENOENT"));
    t.advance(&apc("a=T,f=32,s=10,v=10,i=5", &[0; 8]));
    assert!(reply(&mut t).contains("ENODATA"));
    t.advance(&apc("a=T,f=24,i=5", &[0; 3]));
    assert!(reply(&mut t).contains("EINVAL"), "missing size");
    t.advance(b"\x1b_Ga=T,f=100,i=6;!!!!\x1b\\");
    assert!(reply(&mut t).contains("EINVAL:bad base64"));
    t.advance(&apc("a=T,f=32,s=1,v=1", &[0; 4]));
    assert_eq!(reply(&mut t), "", "no id, no reply");
    t.advance(&apc("a=q,f=32,s=1,v=1,i=31", &[0; 4]));
    assert_eq!(reply(&mut t), "\x1b_Gi=31;OK\x1b\\", "query answers but stores nothing");
    assert!(t.graphics().image(31).is_none());
    t.advance(b"\x1b_Ga=T,t=f,f=100,i=8;L2V0Yy9wYXNzd2Q=\x1b\\"); // /etc/passwd
    assert!(reply(&mut t).contains("EBADPNG"), "files are read, then must decode");
    t.advance(b"\x1b_Ga=T,t=f,i=9;L3Byb2Mvc2VsZi9lbnZpcm9u\x1b\\"); // /proc/self/environ
    assert!(reply(&mut t).contains("EPERM"));
}

#[test]
fn images_scroll_into_history_and_out() {
    let mut t = Terminal::with_scrollback(20, 4, 3);
    t.set_cell_pixel_size(10.0, 20.0);
    t.advance(&apc("a=T,f=32,s=10,v=20,i=1", &rgba(10, 20, [9, 9, 9, 255])));
    t.advance(b"\r\n\r\n\r\n");
    assert_eq!(t.visible_images()[0].row, 0);
    t.advance(b"\r\n");
    assert!(t.visible_images().is_empty(), "scrolled off the screen");
    t.scroll_display(1);
    assert_eq!(t.visible_images()[0].row, 0, "...into history, where it shows");
    t.scroll_display(-1);
    t.advance(b"\r\n\r\n\r\n\r\n");
    assert!(t.graphics().placements().is_empty(), "gone with the history");
    assert!(t.graphics().image(1).is_some(), "an image with an id stays stored");
}

#[test]
fn deletes() {
    let mut t = term();
    let img = rgba(10, 20, [1, 1, 1, 255]);
    t.advance(&apc("a=T,f=32,s=10,v=20,i=1,p=1,C=1,q=2", &img));
    t.advance(b"\x1b[1;5H");
    t.advance(&apc("a=T,f=32,s=10,v=20,i=2,C=1,q=2,z=-5", &img));
    assert_eq!(t.graphics().placements().len(), 2);
    t.advance(b"\x1b_Ga=d,d=z,z=-5\x1b\\");
    assert_eq!(t.graphics().placements().len(), 1);
    assert!(t.graphics().image(2).is_some(), "lowercase keeps data");
    t.advance(b"\x1b_Ga=p,i=2,C=1,q=2\x1b\\");
    t.advance(b"\x1b_Ga=d,d=I,i=2\x1b\\");
    assert!(t.graphics().image(2).is_none(), "uppercase frees data");
    t.advance(b"\x1b_Ga=d,d=p,x=1,y=1\x1b\\");
    assert!(t.graphics().placements().is_empty(), "by cell");
    // Same image + placement id replaces.
    t.advance(&apc("a=t,f=32,s=10,v=20,i=4,q=2", &img));
    t.advance(b"\x1b_Ga=p,i=4,p=9,q=2\x1b\\\x1b_Ga=p,i=4,p=9,q=2\x1b\\");
    assert_eq!(t.graphics().placements().len(), 1);
    // ED 2 clears what is on screen.
    t.advance(b"\x1b[2J");
    assert!(t.graphics().placements().is_empty());
}

#[test]
fn alternate_screen_has_its_own_images() {
    let mut t = term();
    let img = rgba(10, 20, [1, 1, 1, 255]);
    t.advance(&apc("a=T,f=32,s=10,v=20,i=1,q=2", &img));
    t.advance(b"\x1b[?1049h");
    assert!(t.visible_images().is_empty());
    t.advance(&apc("a=T,f=32,s=10,v=20,i=2,q=2", &img));
    assert_eq!(t.visible_images()[0].image.id, 2);
    t.advance(b"\x1b[?1049l");
    let v = t.visible_images();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].image.id, 1, "primary image back; alternate ones dropped");
}

#[test]
fn sixel_places_an_image_and_moves_below_it() {
    let mut t = term();
    t.advance(b"x\x1bPq#1;2;100;0;0#1!12~-!12~-!12~-!12~\x1b\\");
    let v = t.visible_images();
    assert_eq!(v.len(), 1);
    assert_eq!((v[0].image.width, v[0].image.height), (12, 24));
    assert_eq!((v[0].placement.cols, v[0].placement.rows, v[0].placement.col), (2, 2, 1));
    assert_eq!(&v[0].image.rgba[..4], &[255, 0, 0, 255]);
    let c = t.cursor();
    assert_eq!((c.row, c.col), (2, 1), "the line below the image");
    // Redrawing the same spot replaces the old picture instead of piling up.
    t.advance(b"\x1b[1;2H\x1bPq#1~~~~~~~~~~~~\x1b\\\x1b[1;2H\x1bPq#1~~~~~~~~~~~~\x1b\\");
    assert_eq!(t.graphics().images().count(), 2, "the covered one is dropped");
    // DA1 advertises Sixel.
    t.advance(b"\x1b[c");
    assert!(reply(&mut t).contains(";4;"));
}

#[test]
fn size_reports() {
    let mut t = term();
    t.advance(b"\x1b[14t\x1b[16t\x1b[18t");
    assert_eq!(reply(&mut t), "\x1b[4;120;200t\x1b[6;20;10t\x1b[8;6;20t");
    let mut unknown = Terminal::new(20, 6);
    unknown.advance(b"\x1b[16t");
    assert_eq!(reply(&mut unknown), "", "no cell size from the front end: no answer");
}

#[test]
fn reset_forgets_images() {
    let mut t = term();
    t.advance(&apc("a=T,f=32,s=1,v=1,i=1,q=2", &[0; 4]));
    t.advance(b"\x1bc");
    assert!(t.graphics().placements().is_empty() && t.graphics().image(1).is_none());
}

#[test]
fn random_apc_input_never_panics() {
    let mut t = term();
    let mut seed = 12345u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let alphabet = b"aTtpdqfsvimoxywhXYcrzCUIS0123456789=,;-AZaz+/_\x1b\\G";
    for _ in 0..3000 {
        let n = (next() % 60) as usize;
        let body: Vec<u8> =
            (0..n).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect();
        let mut msg = b"\x1b_G".to_vec();
        msg.extend(&body);
        msg.extend(b"\x1b\\");
        t.advance(&msg);
        t.take_responses();
        t.check_invariants().unwrap();
    }
}

#[test]
fn images_move_with_scroll_regions() {
    let mut t = Terminal::with_scrollback(20, 10, 100);
    t.set_cell_pixel_size(10.0, 20.0);
    let img = rgba(10, 20, [1, 1, 1, 255]);
    // One image inside the region (rows 3-7), one outside (row 0).
    t.advance(&apc("a=T,f=32,s=10,v=20,i=1,C=1,q=2", &img));
    t.advance(b"\x1b[6;1H");
    t.advance(&apc("a=T,f=32,s=10,v=20,i=2,C=1,q=2", &img));
    t.advance(b"\x1b[3;8r\x1b[8;1H\n"); // scroll the region up one line
    let rows = |t: &Terminal| {
        let mut v: Vec<(u32, i64)> =
            t.visible_images().iter().map(|v| (v.image.id, v.row)).collect();
        v.sort();
        v
    };
    assert_eq!(rows(&t), [(1, 0), (2, 4)], "only the image in the region moved");
    t.advance(b"\x1b[2T"); // scroll down two (SD) inside the region
    assert_eq!(rows(&t), [(1, 0), (2, 6)]);
    t.advance(b"\x1b[3;1H\x1b[5M"); // delete 5 lines at the region top: pushed out
    assert_eq!(rows(&t), [(1, 0)], "scrolled out of the region");
    // Full-screen scroll down (reverse index at the top) moves images too.
    t.advance(b"\x1b[r\x1b[1;1H\x1bM");
    assert_eq!(rows(&t), [(1, 1)]);
}

#[cfg(target_os = "linux")]
#[test]
fn shared_memory_transfer() {
    // POSIX shm lives in /dev/shm on Linux; write the object there.
    let name = format!("/seraphim-test-{}", std::process::id());
    let path = format!("/dev/shm{name}");
    if std::fs::write(&path, rgba(2, 2, [7, 8, 9, 255])).is_err() {
        return; // no /dev/shm here
    }
    let mut t = term();
    t.advance(format!("\x1b_Ga=T,t=s,f=32,s=2,v=2,i=4;{}\x1b\\", b64(name.as_bytes())).as_bytes());
    assert_eq!(reply(&mut t), "\x1b_Gi=4;OK\x1b\\");
    assert_eq!(&t.graphics().image(4).unwrap().rgba[..4], &[7, 8, 9, 255]);
    assert!(!std::path::Path::new(&path).exists(), "unlinked after reading");
    t.advance(format!("\x1b_Ga=T,t=s,i=5;{}\x1b\\", b64(b"/a/b")).as_bytes());
    assert!(reply(&mut t).contains("EINVAL"));
}

#[test]
fn unicode_placeholders_create_virtual_placements() {
    use seraphim_terminal::graphics::{diacritic_value, DIACRITICS, PLACEHOLDER};
    let mut t = term();
    t.advance(&apc("a=T,U=1,f=32,s=4,v=2,i=42,c=4,r=2,q=2", &rgba(4, 2, [1, 2, 3, 255])));
    assert!(t.visible_images().is_empty(), "virtual: shown only through placeholders");
    assert_eq!((t.cursor().row, t.cursor().col), (0, 0), "no cursor movement");
    let (img, p) = t.graphics().virtual_placement(42, 0).unwrap();
    assert_eq!((img.id, p.cols, p.rows), (42, 4, 2));
    // A placeholder row: fg = image id 42, diacritics row 0 / column 1.
    let d = |i: usize| char::from_u32(DIACRITICS[i]).unwrap();
    t.advance(format!("\x1b[38;5;42m{PLACEHOLDER}{}{}\x1b[m", d(0), d(1)).as_bytes());
    let row = t.screen().row(0);
    assert_eq!(row.cells[0].ch, PLACEHOLDER);
    assert_eq!(row.combining(0), &[d(0), d(1)], "marks stay on the cell");
    assert_eq!(diacritic_value(d(1)), Some(1));
    assert_eq!(diacritic_value('a'), None);
    // Clearing the screen keeps virtual placements; deleting by id doesn't.
    t.advance(b"\x1b[2J");
    assert!(t.graphics().virtual_placement(42, 0).is_some());
    t.advance(b"\x1b_Ga=d,d=I,i=42\x1b\\");
    assert!(t.graphics().virtual_placement(42, 0).is_none());
}

#[test]
fn animation_frames_and_playback() {
    use seraphim_terminal::graphics::PlayState;
    use std::time::Duration;
    let ms = Duration::from_millis;
    let mut t = term();
    let red = rgba(2, 2, [255, 0, 0, 255]);
    t.advance(&apc("a=T,f=32,s=2,v=2,i=9,q=2", &red));
    // Frame 2: blue everywhere, 100 ms. Frame 3: frame 1 plus a
    // half-transparent white pixel at (1,0), blended; frame 4 replaces.
    t.advance(&apc("a=f,i=9,f=32,s=2,v=2,z=100", &rgba(2, 2, [0, 0, 255, 255])));
    assert_eq!(reply(&mut t), "\x1b_Gi=9;OK\x1b\\");
    t.advance(&apc("a=f,i=9,c=1,x=1,y=0,s=1,v=1,f=32,q=2", &[255, 255, 255, 128]));
    t.advance(&apc("a=f,i=9,c=1,X=1,s=1,v=1,f=32,q=2", &[0, 0, 0, 0]));
    // A frame on a background color (Y = RGBA) with no base.
    t.advance(&apc("a=f,i=9,Y=16711935,s=1,v=1,f=32,q=2", &[0, 0, 0, 0]));
    let img = t.graphics().image(9).unwrap();
    assert_eq!(img.frame_count(), 5);
    assert_eq!(&img.frame(1).0[..4], &[0, 0, 255, 255]);
    let f3 = img.frame(2).0;
    assert_eq!(&f3[..4], &[255, 0, 0, 255], "copied from frame 1");
    assert_eq!(&f3[4..8], &[255, 128, 128, 255], "white at half alpha over red");
    assert_eq!(&img.frame(3).0[..4], &[0, 0, 0, 0], "X=1 replaces, alpha too");
    assert_eq!(&img.frame(4).0[4..8], &[0, 255, 0, 255], "Y=0x00ff00ff background");
    assert_eq!(img.anim.gaps, [40, 100, 40, 40, 40], "default gap 40 ms");
    assert!(!img.animating(), "stopped until told to run");

    // Run, looping forever; frame 1 for 40 ms, frame 2 for 100 ms ...
    t.advance(b"\x1b_Ga=a,i=9,s=3,v=1,q=2\x1b\\");
    let img = t.graphics().image(9).unwrap();
    assert_eq!(img.anim.state, PlayState::Running);
    assert!(t.graphics().animating() || t.graphics().placements().is_empty());
    let e = img.anim.epoch;
    assert_eq!(img.frame_at(e + ms(10)), 0);
    assert_eq!(img.frame_at(e + ms(60)), 1);
    assert_eq!(img.frame_at(e + ms(150)), 2);
    assert_eq!(img.frame_at(e + ms(260 + 10)), 0, "loops (period 260 ms)");
    // Two passes then stop on the last frame (v=3 means 2 loops).
    t.advance(b"\x1b_Ga=a,i=9,v=3,c=1,q=2\x1b\\");
    let img = t.graphics().image(9).unwrap();
    let e = img.anim.epoch;
    assert_eq!(img.frame_at(e + ms(260 + 60)), 1, "second pass");
    assert_eq!(img.frame_at(e + ms(600)), 4, "done: last frame");
    // Loading mode waits on the last frame; stop freezes; c picks a frame.
    t.advance(b"\x1b_Ga=a,i=9,s=2,v=1,c=1,q=2\x1b\\");
    let img = t.graphics().image(9).unwrap();
    assert_eq!(img.frame_at(img.anim.epoch + ms(2000)), 4);
    t.advance(b"\x1b_Ga=a,i=9,s=1,c=3,q=2\x1b\\");
    let img = t.graphics().image(9).unwrap();
    assert_eq!(img.frame_at(img.anim.epoch + ms(5000)), 2);
    // Gap change for frame 2 (r=2, z=500).
    t.advance(b"\x1b_Ga=a,i=9,r=2,z=500,q=2\x1b\\");
    assert_eq!(t.graphics().image(9).unwrap().anim.gaps[1], 500);
    // Errors.
    t.advance(&apc("a=f,i=404,f=32,s=1,v=1", &[0; 4]));
    assert!(reply(&mut t).contains("ENOENT"));
    t.advance(b"\x1b_Ga=c,i=9\x1b\\");
    assert!(reply(&mut t).contains("EINVAL"));
}
