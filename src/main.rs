// Ferry — a fast SFTP and S3 browser on GPUI Fast.
//
// The UI thread draws and hands work to the background engine; it never
// blocks and never awaits the network. GPUI only draws when something
// changed, so an idle window costs nothing.

mod ui;

use gpui::{
    App, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size,
};

use ui::workspace::*;

pub fn bind_keys(cx: &mut App) {
    ui::text_input::bind_keys(cx);
    let list = Some("FileList");
    cx.bind_keys([
        KeyBinding::new("up", MoveUp, list),
        KeyBinding::new("down", MoveDown, list),
        KeyBinding::new("pageup", PageUp, list),
        KeyBinding::new("pagedown", PageDown, list),
        KeyBinding::new("home", MoveHome, list),
        KeyBinding::new("end", MoveEnd, list),
        KeyBinding::new("shift-up", ExtendUp, list),
        KeyBinding::new("shift-down", ExtendDown, list),
        KeyBinding::new("shift-home", ExtendHome, list),
        KeyBinding::new("shift-end", ExtendEnd, list),
        KeyBinding::new("enter", Open, list),
        KeyBinding::new("backspace", GoUp, list),
        KeyBinding::new("ctrl-a", SelectAll, list),
        KeyBinding::new("escape", ClearSelection, list),
        KeyBinding::new("f2", Rename, list),
        KeyBinding::new("delete", DeleteSelected, list),
        KeyBinding::new("f3", Download, list),
        KeyBinding::new("ctrl-d", Download, list),
        KeyBinding::new("ctrl-shift-d", DownloadTo, list),
        KeyBinding::new("ctrl-shift-c", CopyUrl, list),
    ]);
    let filter = Some("FilterBar");
    let location = Some("LocationBar");
    let dialog = Some("Dialog");
    cx.bind_keys([
        KeyBinding::new("enter", InputConfirm, filter),
        KeyBinding::new("down", InputDown, filter),
        KeyBinding::new("escape", InputCancel, filter),
        KeyBinding::new("enter", InputConfirm, location),
        KeyBinding::new("escape", InputCancel, location),
        KeyBinding::new("enter", DialogConfirm, dialog),
        KeyBinding::new("escape", DialogCancel, dialog),
        KeyBinding::new("tab", DialogNext, dialog),
        KeyBinding::new("shift-tab", DialogPrev, dialog),
    ]);
    let ctx = Some("Workspace");
    cx.bind_keys([
        KeyBinding::new("ctrl-n", NewConnection, ctx),
        KeyBinding::new("ctrl-w", CloseTab, ctx),
        KeyBinding::new("ctrl-tab", NextTab, ctx),
        KeyBinding::new("ctrl-shift-tab", PrevTab, ctx),
        KeyBinding::new("ctrl-pagedown", NextTab, ctx),
        KeyBinding::new("ctrl-pageup", PrevTab, ctx),
        KeyBinding::new("ctrl-l", EditLocation, ctx),
        KeyBinding::new("f6", EditLocation, ctx),
        KeyBinding::new("ctrl-f", FocusFilter, ctx),
        KeyBinding::new("ctrl-h", ToggleHidden, ctx),
        KeyBinding::new("ctrl-j", ToggleTransfers, ctx),
        KeyBinding::new("ctrl-u", UploadFiles, ctx),
        KeyBinding::new("ctrl-shift-u", UploadFolder, ctx),
        KeyBinding::new("ctrl-shift-n", NewFolder, ctx),
        KeyBinding::new("f5", Refresh, ctx),
        KeyBinding::new("ctrl-r", Refresh, ctx),
        KeyBinding::new("alt-left", GoBack, ctx),
        KeyBinding::new("alt-right", GoForward, ctx),
        KeyBinding::new("alt-up", GoUp, ctx),
        KeyBinding::new("f1", ShowHelp, ctx),
        KeyBinding::new("ctrl-,", ShowSettings, ctx),
        KeyBinding::new("escape", Dismiss, ctx),
        KeyBinding::new("ctrl-q", Quit, None),
    ]);
}

fn open_window(cx: &mut App) {
    let saved = ferry::model::settings::Settings::load().window.sane();
    let bounds = Bounds::centered(None, size(px(saved.width), px(saved.height)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Ferry".into()),
                ..Default::default()
            }),
            app_id: Some("ferry".into()),
            window_min_size: Some(size(px(760.0), px(460.0))),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| Workspace::new(window, cx)),
    )
    .expect("failed to open window");
}

/// When the process started, for the startup probe.
pub static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

fn main() {
    STARTED.get_or_init(std::time::Instant::now);
    init_logging();
    // glibc gives every thread its own malloc arena, and memory freed in one
    // lingers there. The engine threads barely allocate; a few arenas are
    // plenty and keep the footprint down.
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 4);
    }

    #[cfg(feature = "snapshot")]
    if std::env::args().nth(1).as_deref() == Some("--snapshot") {
        ui::snapshot::run();
        return;
    }

    gpui_platform::application()
        .with_assets(ui::assets::Assets)
        .run(move |cx: &mut App| {
            bind_keys(cx);
            cx.on_action(|_: &Quit, cx| cx.quit());
            open_window(cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
}

/// Warnings only, unless `FERRY_LOG` asks for more.
fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("FERRY_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
