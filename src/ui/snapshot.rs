//! Developer tool: render the window offscreen to PNG without opening it.
//!
//!   cargo run --release --features snapshot -- --snapshot OUT.png SCENARIO [light] [size=WxH]
//!
//! Scenarios fill the window with made-up connections, listings and
//! transfers; see `app/fake.rs`. With FERRY_FRAMES=N, also prints
//! main-thread CPU per frame while scrolling.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{AppContext as _, HeadlessAppContext, px, size};

use crate::ui::workspace::Workspace;

pub fn run() {
    isolate();
    if std::env::args().nth(3).as_deref() == Some("live") {
        live();
        return;
    }
    let args: Vec<String> = std::env::args().skip(2).collect();
    let out = PathBuf::from(&args[0]);
    let mut dims = (1320.0, 820.0);
    let mut scenario = String::from("list");
    let mut light = None;
    for a in &args[1..] {
        if let Some(v) = a.strip_prefix("size=") {
            let (w, h) = v.split_once('x').expect("size=WxH");
            dims = (w.parse().unwrap(), h.parse().unwrap());
        } else if a == "light" {
            light = Some(ferry::model::settings::Theme::Light);
        } else if a == "dark" {
            light = Some(ferry::model::settings::Theme::Dark);
        } else {
            scenario = a.clone();
        }
    }
    if let Some(theme) = light {
        let theme = ferry::model::settings::Settings {
            theme,
            ..Default::default()
        };
        let _ = theme.save();
    }

    tracing::debug!("snapshot: text system");
    let text = Arc::new(gpui_wgpu::CosmicTextSystem::new("Noto Sans"));
    tracing::debug!("snapshot: app");
    let mut cx = HeadlessAppContext::with_platform(text, Arc::new(crate::ui::assets::Assets), || {
        gpui_platform::current_headless_renderer()
    });
    cx.update(crate::bind_keys);
    tracing::debug!("snapshot: window");
    let window = cx
        .open_window(size(px(dims.0), px(dims.1)), |window, cx| {
            cx.new(|cx| Workspace::new(window, cx))
        })
        .unwrap();
    let settle = |cx: &mut HeadlessAppContext| {
        for _ in 0..10 {
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        cx.run_until_parked();
    };
    settle(&mut cx);
    let _ = cx.capture_screenshot(window.into());
    cx.update_window(window.into(), |root, window, cx| {
        let view = root.downcast::<Workspace>().unwrap();
        view.update(cx, |this, cx| this.debug_fake(&scenario, window, cx));
    })
    .unwrap();
    settle(&mut cx);
    let _ = cx.capture_screenshot(window.into());
    settle(&mut cx);
    if let Some(n) = std::env::var("FERRY_FRAMES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        frame_bench(&mut cx, window.into(), n);
    }
    let img = cx.capture_screenshot(window.into()).expect("screenshot");
    img.save(&out).expect("save png");
    eprintln!("wrote {}", out.display());
}

fn thread_cpu() -> f64 {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

/// Main-thread CPU per frame while the cursor walks down the list (which
/// scrolls it), and while idle-redrawing.
fn frame_bench(cx: &mut HeadlessAppContext, window: gpui::AnyWindowHandle, n: usize) {
    for (label, step) in [("scroll", true), ("redraw", false)] {
        let mut total = 0.0;
        let mut worst: f64 = 0.0;
        for _ in 0..n {
            let t = thread_cpu();
            cx.update_window(window, |root, window, cx| {
                let view = root.downcast::<Workspace>().unwrap();
                view.update(cx, |this, cx| this.debug_step(step, window, cx));
            })
            .unwrap();
            cx.run_until_parked();
            let _ = cx.capture_screenshot(window);
            let dt = thread_cpu() - t;
            total += dt;
            worst = worst.max(dt);
        }
        eprintln!(
            "{label}: {:.2} ms/frame mean, {:.2} ms worst (incl. offscreen readback)",
            total / n as f64 * 1e3,
            worst * 1e3
        );
    }
}

/// Never touch the real config or keychain-backed bookmarks.
fn isolate() {
    let dir = std::env::temp_dir().join(format!("ferry-snap-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
        std::env::set_var("XDG_CACHE_HOME", dir.join("cache"));
        std::env::remove_var("FERRY_OPEN");
    }
}

/// End-to-end against a real SFTP server:
///
///   FERRY_LIVE=host:port:user:/path/to/key:/remote/dir:/local/mirror/of/dir \
///     ferry --snapshot OUT.png live
///
/// The local mirror is the same directory seen from this machine, so the
/// script can check what each step did. Prints PASS/FAIL per step.
fn live() {
    let spec = std::env::var("FERRY_LIVE").expect("FERRY_LIVE");
    let parts: Vec<&str> = spec.split(':').collect();
    let (host, port, user, key, remote, local) = (
        parts[0].to_owned(),
        parts[1].parse::<u16>().unwrap(),
        parts[2].to_owned(),
        parts[3].to_owned(),
        parts[4].to_owned(),
        PathBuf::from(parts[5]),
    );
    let out = PathBuf::from(std::env::args().nth(2).unwrap());
    // Keep known_hosts and Downloads inside the sandbox too.
    let home = std::env::temp_dir().join(format!("ferry-snap-{}", std::process::id())).join("home");
    std::fs::create_dir_all(home.join("Downloads")).unwrap();
    unsafe { std::env::set_var("HOME", &home) };

    let text = Arc::new(gpui_wgpu::CosmicTextSystem::new("Noto Sans"));
    let mut cx = HeadlessAppContext::with_platform(text, Arc::new(crate::ui::assets::Assets), || {
        gpui_platform::current_headless_renderer()
    });
    cx.update(crate::bind_keys);
    let window = cx
        .open_window(size(px(1320.), px(820.)), |window, cx| {
            cx.new(|cx| Workspace::new(window, cx))
        })
        .unwrap();
    let handle: gpui::AnyWindowHandle = window.into();
    let with = |cx: &mut HeadlessAppContext, f: &mut dyn FnMut(&mut Workspace, &mut gpui::Window, &mut gpui::Context<Workspace>)| {
        cx.update_window(handle, |root, window, cx| {
            let view = root.downcast::<Workspace>().unwrap();
            view.update(cx, |this, cx| f(this, window, cx));
        })
        .unwrap();
    };
    let status = |cx: &mut HeadlessAppContext| {
        let mut s = None;
        with(cx, &mut |this, _, _| s = Some(this.debug_live_status()));
        s.unwrap()
    };
    // Waits until connected, nothing loading, and no transfers running.
    let settle = |cx: &mut HeadlessAppContext| {
        let started = std::time::Instant::now();
        loop {
            cx.run_until_parked();
            let s = status(cx);
            if s.phase != "connecting" && s.phase != "unlocking" && !s.loading && !s.busy {
                // One more beat for a trailing refresh.
                std::thread::sleep(std::time::Duration::from_millis(150));
                cx.run_until_parked();
                let s = status(cx);
                if !s.loading && !s.busy {
                    return s;
                }
            }
            if started.elapsed().as_secs() > 20 {
                return s;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
    };
    let mut failures = 0;
    let mut check = |name: &str, ok: bool, detail: String| {
        if ok {
            eprintln!("PASS {name}");
        } else {
            failures += 1;
            eprintln!("FAIL {name}: {detail}");
        }
    };
    let step = |cx: &mut HeadlessAppContext, what: &str, arg: &str| {
        with(cx, &mut |this, w, cx| this.debug_live_do(what, arg, w, cx));
    };

    let t = std::time::Instant::now();
    with(&mut cx, &mut |this, w, cx| {
        this.debug_live_open(&host, port, &user, &key, &remote, w, cx)
    });
    let s = settle(&mut cx);
    check(
        "connect and list",
        s.phase == "ready" && s.names.iter().any(|n| n == "big.bin"),
        format!("{} {:?}", s.phase, s.names),
    );
    eprintln!("     connected and listed {} entries in {:?}", s.names.len(), t.elapsed());

    step(&mut cx, "mkdir", "made by test");
    let s = settle(&mut cx);
    check(
        "new folder",
        local.join("made by test").is_dir() && s.names.iter().any(|n| n == "made by test"),
        s.toast.clone(),
    );

    step(&mut cx, "rename", "made by test>renamed");
    let s = settle(&mut cx);
    check(
        "rename",
        local.join("renamed").is_dir() && s.names.iter().any(|n| n == "renamed"),
        s.toast.clone(),
    );

    step(&mut cx, "download", "big.bin");
    let s = settle(&mut cx);
    let same = std::fs::read(home.join("Downloads/big.bin")).ok() == std::fs::read(local.join("big.bin")).ok();
    check("download a file", same, s.toast.clone());

    step(&mut cx, "download", "docs");
    let s = settle(&mut cx);
    check(
        "download a folder",
        home.join("Downloads/docs/readme.md").is_file(),
        s.toast.clone(),
    );

    let upload = home.join("upload me.txt");
    std::fs::write(&upload, b"uploaded by the live test").unwrap();
    step(&mut cx, "upload", upload.to_str().unwrap());
    let s = settle(&mut cx);
    check(
        "upload",
        std::fs::read(local.join("upload me.txt")).ok().as_deref() == Some(&b"uploaded by the live test"[..])
            && s.names.iter().any(|n| n == "upload me.txt"),
        format!("{} {:?}", s.toast, s.names.len()),
    );

    step(&mut cx, "delete", "renamed");
    let s = settle(&mut cx);
    check("delete", !local.join("renamed").exists(), s.toast.clone());
    step(&mut cx, "delete", "upload me.txt");
    settle(&mut cx);

    step(&mut cx, "open", "link-to-docs");
    let s = settle(&mut cx);
    check(
        "enter a symlinked folder",
        s.names == ["readme.md"],
        format!("{} {:?}", s.path, s.names),
    );
    step(&mut cx, "up", "");
    let s = settle(&mut cx);
    check("go up", s.path == remote, s.path.clone());

    step(&mut cx, "filter", "note-1");
    let s = settle(&mut cx);
    check("filter", s.names.len() == 11, format!("{:?}", s.names));
    let img = cx.capture_screenshot(handle).expect("screenshot");
    img.save(&out).expect("save png");
    eprintln!("wrote {}", out.display());
    if failures > 0 {
        eprintln!("{failures} FAILED");
        std::process::exit(1);
    }
    eprintln!("all passed");
}
