//! Renders the real UI into ratatui's TestBackend for every state, asserts
//! on what's on screen, and (with SNAP_DIR set) dumps each frame as JSON so
//! it can be turned into screenshots.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

use courierqr_core::{FrameConfig, SealKey, Transmission};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;

use super::receive::{self, Msg, Phase};
use super::{App, Opts, SourceSpec, logo};
use crate::camera::CameraInfo;
use crate::render::{Grid, Layout as QrLayout};
use courierqr_core::secrecy::ExposeSecret;
use courierqr_core::{OpenKey, Receiver};

const W: u16 = 160;
const H: u16 = 46;

fn render(app: &mut App) -> ratatui::buffer::Buffer {
    render_at(app, W, H)
}

fn render_at(app: &mut App, w: u16, h: u16) -> ratatui::buffer::Buffer {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| app.draw(f)).unwrap();
    t.backend().buffer().clone()
}

fn text(buf: &ratatui::buffer::Buffer) -> String {
    let mut s = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            s.push_str(buf[(x, y)].symbol());
        }
        s.push('\n');
    }
    s
}

fn hex(c: Color, default: &str) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Reset => default.into(),
        Color::Black => "#000000".into(),
        Color::White => "#ffffff".into(),
        _ => default.into(),
    }
}

/// With SNAP_DIR set, write the frame as JSON for the screenshot renderer.
fn dump(buf: &ratatui::buffer::Buffer, name: &str) {
    let Some(dir) = std::env::var_os("SNAP_DIR") else {
        return;
    };
    let mut out = String::from("{\"w\":");
    out += &format!("{},\"h\":{},\"cells\":[", buf.area.width, buf.area.height);
    for (i, c) in buf.content().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let sym = c.symbol().replace('\\', "\\\\").replace('"', "\\\"");
        let bold = c.modifier.contains(ratatui::style::Modifier::BOLD);
        out += &format!(
            "[\"{sym}\",\"{}\",\"{}\",{}]",
            hex(c.fg, "#e8ebef"),
            hex(c.bg, "#0d0f12"),
            bold as u8
        );
    }
    out += "]}";
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(Path::new(&dir).join(format!("{name}.json")), out).unwrap();
}

fn key(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn typ(app: &mut App, s: &str) {
    for c in s.chars() {
        key(app, KeyCode::Char(c));
    }
}

/// A camera-like recording of a 2x2 grid of QR codes for `tx`.
fn recording(tx: &Transmission, path: &Path) {
    let layout = QrLayout::fit(15, Grid::from_count(4).unwrap(), 640, 640).unwrap();
    let (w, h) = (960usize, 720usize);
    let mut f = std::fs::File::create(path).unwrap();
    writeln!(f, "YUV4MPEG2 W{w} H{h} F30:1 Ip A1:1 C420jpeg").unwrap();
    let uv = vec![128u8; (w / 2) * (h / 2) * 2];
    let mut stream = tx.frames();
    for i in 0..60 {
        let frames: Vec<Vec<u8>> = stream.by_ref().take(4).collect();
        let img = layout.render(&frames).unwrap();
        // Dim room, the codes a bit right of centre, mild noise.
        let mut px = vec![40u8; w * h];
        let (ox, oy) = (220 + (i % 5) * 3, (h - img.height) / 2);
        for y in 0..img.height {
            for x in 0..img.width {
                let v = img.pixels[y * img.width + x] as u32 * 200 / 255 + 30;
                px[(oy + y) * w + ox + x] =
                    (v as u8).wrapping_add(((x * 7 + y * 13 + i) % 9) as u8);
            }
        }
        f.write_all(b"FRAME\n").unwrap();
        f.write_all(&px).unwrap();
        f.write_all(&uv).unwrap();
    }
}

/// Feed worker messages into the app until `until` says stop.
fn drive(
    app: &mut App,
    rx: &mpsc::Receiver<Msg>,
    mut until: impl FnMut(&App) -> bool,
) -> Option<Box<courierqr_core::Receiver>> {
    while let Ok(m) = rx.recv() {
        match m {
            Msg::Opened(i) => app.receive.info = Some(*i),
            Msg::Frame(i) => {
                app.receive.apply(*i);
                if until(app) {
                    return None;
                }
            }
            Msg::Complete(r) => return Some(r),
            Msg::Error(e) => panic!("worker error: {e}"),
            Msg::Window(_) => {}
        }
    }
    None
}

#[test]
fn every_screen_renders_and_works() {
    let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("cqr-tui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("dl")).unwrap();
    // SAFETY: serialized by ENV_LOCK.
    unsafe { std::env::set_var("COURIERQR_CONFIG_DIR", dir.join("cfg")) };

    // A note big enough to need a dozen-plus pieces (varied text compresses less).
    let mut note = String::from(
        "Typed on my phone:\n- buy milk\n- call Andy\n- check the Kalshi fills\n\nField log:\n",
    );
    let mut x = 0x9E37_79B9u32;
    for i in 0..400 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        note += &format!(
            "{i:03} station {:08x} reading {:.1}F gust {}\n",
            x,
            (x % 900) as f64 / 10.0 + 20.0,
            x % 37
        );
    }
    let own_pk = crate::pair::own_public_key_quiet().unwrap().0;

    let new_app = |file: &PathBuf| {
        App::new(Opts {
            camera: None,
            camera_file: Some(file.clone()),
            out: dir.join("dl"),
        })
    };

    // ---- 1. idle, each logo
    let rec = dir.join("paired.y4m");
    let tx = Transmission::new(
        "phone-note.md",
        note.as_bytes(),
        &SealKey::recipient(own_pk.clone()),
        FrameConfig {
            max_frame_bytes: crate::render::byte_capacity(15),
        },
    )
    .unwrap();
    recording(&tx, &rec);
    let mut app = new_app(&rec);
    assert_eq!(app.tab, super::Tab::Send, "opens on Send (tab 1)");
    key(&mut app, KeyCode::Char('2'));
    // The big Courier QR mark is the logo; narrow terminals fall back to smaller marks.
    let buf = render(&mut app);
    assert!(
        text(&buf).contains(logo::LOGOS[0].lines[0].trim_end()),
        "Courier QR logo drawn"
    );
    dump(&buf, "receive-idle");
    let narrow = text(&render_at(&mut app, 100, H));
    assert!(
        logo::LOGOS[1..]
            .iter()
            .any(|l| narrow.contains(l.lines[0].trim_end())),
        "narrow fallback logo drawn"
    );
    let t = text(&render(&mut app));
    assert!(t.contains("Press Enter to start the camera"));
    assert!(t.contains("this device"));

    // ---- 2. scanning a transfer sealed to this computer, mid-way
    app.receive.phase = Phase::Scanning;
    let (txc, rxc) = mpsc::channel();
    let stop = AtomicBool::new(false);
    let rec2 = rec.clone();
    std::thread::scope(|s| {
        s.spawn(|| receive::work(SourceSpec::File(rec2), &txc, &stop).unwrap());
        let half = drive(&mut app, &rxc, |a| {
            a.receive.needed > 0 && a.receive.received * 2 >= a.receive.needed
        });
        assert!(half.is_none());
        assert!(
            app.receive.needed >= 8,
            "want a multi-piece transfer, got {}",
            app.receive.needed
        );
        app.receive.started = Some(std::time::Instant::now() - std::time::Duration::from_secs(9));
        app.receive.poll();
        // Make the pickup sparkline look like a live session.
        for (i, v) in [0, 0, 2, 4, 3, 5, 4, 6, 4, 5, 3, 4].iter().enumerate() {
            app.receive.rate[48 + i] = *v;
        }
        let buf = render(&mut app);
        let t = text(&buf);
        assert!(t.contains("receiving") && t.contains("pieces"), "{t}");
        assert!(
            t.contains("camera details") && t.contains("960×720") && t.contains("0.7 MP"),
            "{t}"
        );
        assert!(t.contains("fps measured") && t.contains("ms/frame"), "{t}");
        assert!(
            app.receive.capture_fps > 5.0,
            "fps {}",
            app.receive.capture_fps
        );
        dump(&buf, "receive-scanning");
        // How the panel reads with a real webcam (values typical of a C920).
        let real = app.receive.info.replace(CameraInfo {
            device: "/dev/video0".into(),
            card: "HD Pro Webcam C920".into(),
            driver: "uvcvideo".into(),
            bus: "usb-0000:00:14.0-2".into(),
            format: "MJPG".into(),
            width: 1280,
            height: 720,
            fps: Some(30.0),
            max: Some((1920, 1080)),
            formats: vec!["YUYV".into(), "MJPG".into()],
            autofocus: Some(true),
        });
        let (fps, ms) = (app.receive.capture_fps, app.receive.decode_ms);
        app.receive.capture_fps = 29.7;
        app.receive.decode_ms = 21.0;
        let buf = render(&mut app);
        let t = text(&buf);
        assert!(
            t.contains("C920 · 1280×720 MJPG · 30 fps"),
            "header summary: {t}"
        );
        assert!(t.contains("0.9 MP") && t.contains("2.1 MP") && t.contains("autofocus: yes"));
        dump(&buf, "receive-webcam");
        app.receive.info = real;
        app.receive.capture_fps = fps;
        app.receive.decode_ms = ms;
        let done = drive(&mut app, &rxc, |_| false).expect("transfer completes");
        receive::unlock_with_own_key(&mut app, done);
    });
    match &app.receive.phase {
        Phase::Done { name, path, .. } => {
            assert_eq!(name, "phone-note.md");
            assert_eq!(std::fs::read_to_string(path).unwrap(), note);
        }
        _ => panic!("expected auto-unlock with this computer's key"),
    }
    let buf = render(&mut app);
    assert!(text(&buf).contains("received") && text(&buf).contains("buy milk"));
    dump(&buf, "receive-done");

    // ---- 3. passphrase transfer: unlock prompt, wrong then right
    let rec3 = dir.join("pass.y4m");
    let seal = SealKey::Passphrase {
        passphrase: "tui-test-pass-123".to_string().into(),
        work_factor: 10,
    };
    let tx3 = Transmission::new(
        "note.txt",
        b"passphrase mode works",
        &seal,
        FrameConfig {
            max_frame_bytes: crate::render::byte_capacity(15),
        },
    )
    .unwrap();
    recording(&tx3, &rec3);
    let mut app = new_app(&rec3);
    app.tab = super::Tab::Receive;
    app.receive.phase = Phase::Scanning;
    let (txc, rxc) = mpsc::channel();
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| receive::work(SourceSpec::File(rec3.clone()), &txc, &stop).unwrap());
        let done = drive(&mut app, &rxc, |_| false).unwrap();
        receive::unlock_with_own_key(&mut app, done);
    });
    assert!(matches!(app.receive.phase, Phase::Unlock { .. }));
    typ(&mut app, "wrong-pass");
    key(&mut app, KeyCode::Enter);
    let buf = render(&mut app);
    assert!(text(&buf).contains("Wrong passphrase"));
    dump(&buf, "receive-unlock");
    typ(&mut app, "tui-test-pass-123");
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.receive.phase, Phase::Done { .. }));

    // ---- 4. devices: add flow with fingerprint confirm, list, delete
    key(&mut app, KeyCode::Char('3'));
    let phone_keys = courierqr_core::generate_device_keys();
    let phone = phone_keys.public.clone();
    let fp = courierqr_core::fingerprint(&phone).unwrap();
    key(&mut app, KeyCode::Char('a'));
    typ(&mut app, &phone);
    let buf = render(&mut app);
    assert!(text(&buf).contains("pair a phone"));
    dump(&buf, "devices-add");
    key(&mut app, KeyCode::Enter);
    let buf = render(&mut app);
    assert!(text(&buf).contains(&fp));
    dump(&buf, "devices-confirm");
    key(&mut app, KeyCode::Char('y'));
    crate::pair::add_quiet("tablet", &courierqr_core::generate_device_keys().public).unwrap();
    app.devices.reload();
    let buf = render(&mut app);
    let t = text(&buf);
    assert!(t.contains("phone") && t.contains("tablet") && t.contains(&fp));
    assert!(
        t.contains(&courierqr_core::fingerprint(&own_pk).unwrap()),
        "own fingerprint shown"
    );
    dump(&buf, "devices");
    // A typo'd key is refused before any confirmation.
    key(&mut app, KeyCode::Char('a'));
    typ(&mut app, "laptop");
    key(&mut app, KeyCode::Tab);
    typ(&mut app, "age1notarealkey");
    key(&mut app, KeyCode::Enter);
    assert!(text(&render(&mut app)).contains("valid age1"));
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Char('d'));
    key(&mut app, KeyCode::Char('d'));
    assert_eq!(crate::pair::load().unwrap().len(), 1);

    // ---- 5. command palette
    app.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
    typ(&mut app, "go");
    let buf = render(&mut app);
    assert!(text(&buf).contains("command palette") && text(&buf).contains("Go to Send"));
    dump(&buf, "palette");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.tab, super::Tab::Send);

    // ---- 6. send: type a note, the terminal shows it as QR codes, and
    // those exact on-screen codes decode back to the note.
    assert!(text(&render(&mut app)).contains("Press Enter and type"));
    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert!(text(&render(&mut app)).contains("type something to send first"));
    key(&mut app, KeyCode::Enter);
    let typed = "Gate code 4471#\nWifi: orbit-cactus\nCall Andy after 5.";
    for (i, line) in typed.split('\n').enumerate() {
        if i > 0 {
            key(&mut app, KeyCode::Enter);
        }
        typ(&mut app, line);
    }
    // A zero-width space (a classic copy-tracking watermark) rides along in
    // the paste; it must not survive to the receiver.
    super::send::on_paste(&mut app, "\npasted: 1\u{200B} 2 3");
    assert_eq!(app.send.text, format!("{typed}\npasted: 1\u{200B} 2 3"));
    let typed = format!("{typed}\npasted: 1 2 3");
    let buf = render(&mut app);
    let t = text(&buf);
    assert!(t.contains("send to") && t.contains("phone") && t.contains("Gate code 4471#"));
    assert!(
        t.contains("phone only") && t.contains("stripped automatically") && t.contains("8 codes/s")
    );
    assert!(t.contains("post-quantum") && t.contains("ML-KEM-768 + X25519 hybrid"));
    dump(&buf, "send-compose");
    // Digits typed into the message stay in the message (no tab switch).
    assert_eq!(app.tab, super::Tab::Send);

    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert!(app.send.live.is_some(), "{:?}", app.send.error);
    let buf = render(&mut app);
    assert!(text(&buf).contains("SENDING"));
    assert!(
        text(&buf).contains("removed 1 invisible character"),
        "{}",
        text(&buf)
    );
    dump(&buf, "send-live");

    let mut rx = Receiver::new();
    let mut shown = 0;
    while !rx.is_complete() {
        shown += 1;
        assert!(shown < 200, "terminal QR stream never completed");
        let buf = render(&mut app);
        // Each cell is one module wide and two tall; draw it at 4 px/module.
        let (s, w, h) = (
            4usize,
            buf.area.width as usize,
            buf.area.height as usize * 2,
        );
        let mut px = vec![255u8; w * s * h * s];
        for cy in 0..buf.area.height {
            for cx in 0..buf.area.width {
                let c = &buf[(cx, cy)];
                if c.symbol() != "▀" {
                    continue;
                }
                for (half, col) in [(0, c.fg), (1, c.bg)] {
                    let v = super::rgb(col).0;
                    let my = cy as usize * 2 + half;
                    for y in 0..s {
                        let row = (my * s + y) * w * s;
                        px[row + cx as usize * s..row + cx as usize * s + s].fill(v);
                    }
                }
            }
        }
        for p in crate::decode::decode_all(&px, w * s, h * s) {
            let _ = rx.push(&p);
        }
        app.send.force_next();
    }
    let secret = phone_keys.secret.expose_secret().to_string();
    let got = rx.open(&OpenKey::identity(secret)).unwrap().unwrap();
    assert_eq!(got.data, typed.as_bytes());
    assert_eq!(got.name, "note.txt", "no timestamp in the name");
    assert_eq!(app.send.method, courierqr_core::Method::PostQuantum);
    key(&mut app, KeyCode::Esc);
    assert!(app.send.live.is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn locked_vault_shows_unlock_gate_then_opens() {
    let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("cqr-tui-unlock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("dl")).unwrap();
    // SAFETY: serialized by ENV_LOCK.
    unsafe { std::env::set_var("COURIERQR_CONFIG_DIR", dir.join("cfg")) };
    unsafe { std::env::remove_var("COURIERQR_RECOVERY_CODE") };
    unsafe { std::env::remove_var("COURIERQR_ACCOUNT_NUMBER") };

    // Create and lock this computer's identity.
    crate::pair::own_public_key_quiet().unwrap();
    let code = courierqr_core::recovery_code();
    crate::pair::vault_enable_with(&code).unwrap();

    let mut app = App::new(Opts {
        camera: None,
        camera_file: None,
        out: dir.join("dl"),
    });
    assert!(
        matches!(app.gate, Some(super::Gate::Unlock { .. })),
        "locked vault prompts for a code"
    );
    assert!(app.own_key.is_none(), "keys stay closed until unlocked");
    let buf = render(&mut app);
    assert!(text(&buf).contains("keys locked") && text(&buf).contains("recovery code"));
    dump(&buf, "unlock");

    // Wrong code: error, still locked.
    typ(&mut app, "WRONG-CODE-00000-00000-00000");
    key(&mut app, KeyCode::Enter);
    assert!(app.gate.is_some());
    assert!(text(&render(&mut app)).contains("Wrong account number"));

    // Right code, pasted (right-click / Ctrl+Shift+V arrive as one paste,
    // here with a trailing newline): opens, keys load, gate gone.
    app.paste(&format!("{code}\n"));
    key(&mut app, KeyCode::Enter);
    assert!(app.gate.is_none(), "legacy recovery code still unlocks");
    assert!(app.own_key.is_some(), "keys available after unlock");

    unsafe { std::env::remove_var("COURIERQR_RECOVERY_CODE") };
    unsafe { std::env::remove_var("COURIERQR_ACCOUNT_NUMBER") };
    let _ = std::fs::remove_dir_all(dir);
}

/// The account number and seed on the confirm screen.
fn confirm_secrets(app: &App) -> (String, Option<String>) {
    match &app.gate {
        Some(super::Gate::Confirm { pending, .. }) => (
            pending.account_number(),
            pending
                .recovery_seed()
                .map(|s| s.expose_secret().to_string()),
        ),
        _ => panic!("expected the confirm screen"),
    }
}

#[test]
fn first_run_sets_up_an_account_then_unlocks_and_restores() {
    let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let base = std::env::temp_dir().join(format!("cqr-tui-setup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("dl")).unwrap();
    // SAFETY: serialized by ENV_LOCK.
    unsafe { std::env::set_var("COURIERQR_CONFIG_DIR", base.join("a")) };
    unsafe { std::env::remove_var("COURIERQR_RECOVERY_CODE") };
    unsafe { std::env::remove_var("COURIERQR_ACCOUNT_NUMBER") };
    let new_app = || {
        App::new(Opts {
            camera: None,
            camera_file: None,
            out: base.join("dl"),
        })
    };

    // ---- first run: welcome, not silent key generation
    let mut app = new_app();
    assert!(matches!(app.gate, Some(super::Gate::Welcome { .. })));
    assert!(app.own_key.is_none());
    assert!(!crate::pair::has_identity().unwrap(), "no keys made yet");
    let buf = render(&mut app);
    assert!(text(&buf).contains("Create a new account"));
    assert!(text(&buf).contains("Restore from recovery seed"));
    dump(&buf, "setup-welcome");

    // ---- new account → device name → confirm
    key(&mut app, KeyCode::Enter);
    assert!(text(&render(&mut app)).contains("Name this device"));
    dump(&render(&mut app), "setup-label");
    key(&mut app, KeyCode::Enter); // default "desktop"
    let (number, seed) = confirm_secrets(&app);
    let seed = seed.expect("new account shows its seed");
    let buf = render(&mut app);
    assert!(text(&buf).contains(&number) && text(&buf).contains(&seed));
    dump(&buf, "setup-confirm");
    assert!(
        !crate::pair::has_identity().unwrap(),
        "nothing written before confirm"
    );

    // Wrong confirmation: stays put.
    typ(&mut app, "0000000000000000");
    key(&mut app, KeyCode::Enter);
    assert!(text(&render(&mut app)).contains("doesn't match"));
    // Right one (typed lowercase, with dashes): keys written, locked, open.
    typ(&mut app, &number.to_lowercase());
    key(&mut app, KeyCode::Enter);
    assert!(app.gate.is_none(), "setup finished");
    let (pk1, _) = app.own_key.clone().expect("keys loaded after setup");
    assert!(crate::pair::vault_is_locked().unwrap(), "encrypted at rest");

    // ---- next launch: unlock with the account number
    unsafe { std::env::remove_var("COURIERQR_ACCOUNT_NUMBER") };
    let mut app = new_app();
    assert!(matches!(app.gate, Some(super::Gate::Unlock { .. })));
    let buf = render(&mut app);
    assert!(text(&buf).contains("account number"));
    dump(&buf, "unlock-account");
    typ(&mut app, "FFFF-FFFF-FFFF-FFFF");
    key(&mut app, KeyCode::Enter);
    assert!(text(&render(&mut app)).contains("Wrong account number"));
    app.paste(&number); // pasted account number, dashes and all
    key(&mut app, KeyCode::Enter);
    assert!(app.gate.is_none());
    assert_eq!(app.own_key.as_ref().unwrap().0, pk1);

    // ---- replacement device: restore from the seed
    unsafe { std::env::set_var("COURIERQR_CONFIG_DIR", base.join("b")) };
    unsafe { std::env::remove_var("COURIERQR_ACCOUNT_NUMBER") };
    let mut app = new_app();
    key(&mut app, KeyCode::Char('2'));
    key(&mut app, KeyCode::Enter);
    assert!(text(&render(&mut app)).contains("same name"));
    key(&mut app, KeyCode::Enter); // "desktop", same as before
    typ(&mut app, "not-a-seed");
    key(&mut app, KeyCode::Enter);
    assert!(matches!(
        app.gate,
        Some(super::Gate::Seed { error: Some(_), .. })
    ));
    for _ in 0..40 {
        key(&mut app, KeyCode::Backspace);
    }
    typ(&mut app, &seed);
    dump(&render(&mut app), "restore-seed");
    key(&mut app, KeyCode::Enter);
    let (number2, shown_seed) = confirm_secrets(&app);
    assert!(shown_seed.is_none(), "restore doesn't re-show the seed");
    assert_ne!(number2, number, "fresh account number per device");
    typ(&mut app, &number2);
    key(&mut app, KeyCode::Enter);
    assert!(app.gate.is_none());
    assert_eq!(app.own_key.as_ref().unwrap().0, pk1, "same keys restored");

    // ---- "continue without an account" keeps the old standalone path
    unsafe { std::env::set_var("COURIERQR_CONFIG_DIR", base.join("c")) };
    unsafe { std::env::remove_var("COURIERQR_ACCOUNT_NUMBER") };
    let mut app = new_app();
    key(&mut app, KeyCode::Char('3'));
    key(&mut app, KeyCode::Enter);
    assert!(app.gate.is_none() && app.own_key.is_some());
    assert!(!crate::pair::vault_is_locked().unwrap());

    unsafe { std::env::remove_var("COURIERQR_ACCOUNT_NUMBER") };
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn receive_tab_plays_the_public_feed() {
    let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("cqr-tui-feed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("dl")).unwrap();
    // SAFETY: serialized by ENV_LOCK.
    unsafe { std::env::set_var("COURIERQR_CONFIG_DIR", dir.join("cfg")) };
    unsafe { std::env::remove_var("COURIERQR_RECOVERY_CODE") };
    unsafe { std::env::remove_var("COURIERQR_ACCOUNT_NUMBER") };
    crate::pair::own_public_key_quiet().unwrap(); // past first-run setup

    let mut app = App::new(Opts {
        camera: None,
        camera_file: None,
        out: dir.join("dl"),
    });
    key(&mut app, KeyCode::Char('2'));
    app.tick();
    let t = text(&render(&mut app));
    assert!(t.contains("Press Enter to start the camera") && t.contains("no messages yet"));

    // Two messages for someone else land on this gateway's feed.
    let them = courierqr_core::generate_device_keys();
    let key_for_them =
        SealKey::for_device(&them.public, courierqr_core::Method::PostQuantum).unwrap();
    for note in ["first message", "second message"] {
        let (env, _) =
            courierqr_core::Envelope::seal_file("n.txt", note.as_bytes(), &key_for_them).unwrap();
        assert!(crate::feed::add(&env).unwrap());
    }
    app.receive.feed.force_sync();
    app.tick();
    let _ = render(&mut app); // sizes the code to the panel
    app.receive.feed.force_sync();
    app.tick();
    assert_eq!(app.receive.feed.len(), 2);
    assert!(app.receive.feed.code.is_some(), "a code is on screen");
    let buf = render(&mut app);
    let t = text(&buf);
    assert!(t.contains("PUBLIC FEED") && t.contains("2 messages"), "{t}");
    dump(&buf, "receive-feed");

    // Starting the camera takes the panel back; the feed resumes after.
    assert!(t.contains("public feed"));

    let _ = std::fs::remove_dir_all(&dir);
}
