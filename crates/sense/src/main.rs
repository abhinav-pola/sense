mod agent_launch;
#[cfg(feature = "automation")]
mod automation;
mod chat;
mod fuzzy;
mod library;
mod model_picker;
mod models;
mod panel;
mod snapshot;
mod thumb;
mod ui_server;
mod view;
mod workspace;
mod worktree;

use gpui_kit::assets::Assets;
use gpui_kit::component::{Theme, ThemeMode, TitleBar};
use gpui_kit::*;

actions!(sense, [TextBigger, TextSmaller, TextActualSize]);

/// The text sizes cmd +/− step through, in points; the theme's own is 16.
const TEXT_SIZES: std::ops::RangeInclusive<f32> = 10.0..=28.0;
const TEXT_SIZE_DEFAULT: f32 = 16.;

fn main() {
    // fx starts this same binary as the MCP server for the panel's tools.
    if std::env::args().nth(1).as_deref() == Some("--ui-server") {
        ui_server::serve();
        return;
    }
    install_panic_log();
    gpui_kit::application().with_assets(Assets).run(|cx| {
        gpui_kit::init(cx);
        load_fonts(cx);
        // The system's look until the window can say what the user chose.
        apply_mode(cx.window_appearance().into(), cx);
        // Background mode is for automated runs on a machine someone is using:
        // the window opens without taking focus or activating the app.
        let background = std::env::var_os("SENSE_BACKGROUND").is_some();
        if !background {
            cx.activate(true);
        }

        text_size_keys(cx);

        let options = WindowOptions {
            titlebar: Some(TitleBar::title_bar_options()),
            window_bounds: Some(WindowBounds::centered(size(px(1280.), px(820.)), cx)),
            window_min_size: Some(size(px(720.), px(480.))),
            window_background: WindowBackgroundAppearance::Blurred,
            focus: !background,
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            // Startup benchmarks time launch through the first rendered frame.
            // Callbacks for the next frame run before it is drawn, so the one
            // registered there fires once the first frame has been presented.
            // A still window draws no further frames on its own, so the
            // second one is requested.
            if std::env::var_os("SENSE_EXIT_AFTER_FIRST_FRAME").is_some() {
                window.on_next_frame(|window, _| {
                    window.on_next_frame(|_, cx| cx.quit());
                    window.refresh();
                });
                // A benchmark that hangs must fail, not stall the run.
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    eprintln!("sense: no frame within 5s of launch");
                    std::process::exit(1);
                });
            }
            cx.new(|cx| workspace::Workspace::new(window, cx))
        })
        .expect("open main window");
    });
}

/// JetBrains Mono, the "NL" cut: the same letters with no ligatures, so
/// code reads as typed (`!=` stays two characters) and shaping does less.
const FONT: &str = "JetBrains Mono NL";

/// The interface font ships inside the binary: no lookup among installed
/// fonts, and the same look on every machine.
fn load_fonts(cx: &mut App) {
    let fonts = [
        include_bytes!("../assets/fonts/JetBrainsMonoNL-Regular.ttf").as_slice(),
        include_bytes!("../assets/fonts/JetBrainsMonoNL-Medium.ttf"),
        include_bytes!("../assets/fonts/JetBrainsMonoNL-SemiBold.ttf"),
        include_bytes!("../assets/fonts/JetBrainsMonoNL-Bold.ttf"),
    ];
    let fonts = fonts.into_iter().map(std::borrow::Cow::Borrowed).collect();
    if let Err(err) = cx.text_system().add_fonts(fonts) {
        eprintln!("sense: couldn't load the bundled font: {err:#}");
    }
}

/// Switches to light or dark, keeping what holds across both: the bundled
/// font, the text size, and a clear window background, so the blur behind
/// it shows where a pane lets it (panes paint their own backgrounds).
pub fn apply_mode(mode: ThemeMode, cx: &mut App) {
    let size = if cx.has_global::<Theme>() {
        Theme::global(cx).font_size
    } else {
        px(TEXT_SIZE_DEFAULT)
    };
    Theme::change(mode, None, cx);
    Theme::update(cx, |theme| {
        theme.font_family = FONT.into();
        theme.mono_font_family = FONT.into();
        theme.font_size = size;
        theme.mono_font_size = size;
        theme.background = theme.background.alpha(0.);
    });
}

/// cmd + / cmd − / cmd 0: the size of all text, everywhere. Handled for the
/// whole app, so they work whatever has focus (or nothing does).
fn text_size_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-=", TextBigger, None),
        KeyBinding::new("cmd-+", TextBigger, None),
        KeyBinding::new("cmd--", TextSmaller, None),
        KeyBinding::new("cmd-0", TextActualSize, None),
    ]);
    cx.on_action(|_: &TextBigger, cx| set_text_size(cx, |size| size + 1.));
    cx.on_action(|_: &TextSmaller, cx| set_text_size(cx, |size| size - 1.));
    cx.on_action(|_: &TextActualSize, cx| set_text_size(cx, |_| TEXT_SIZE_DEFAULT));
}

pub fn set_text_size(cx: &mut App, change: impl FnOnce(f32) -> f32) {
    let size = change(f32::from(Theme::global(cx).font_size))
        .clamp(*TEXT_SIZES.start(), *TEXT_SIZES.end());
    if size != f32::from(Theme::global(cx).font_size) {
        Theme::update(cx, |theme| {
            theme.font_size = px(size);
            theme.mono_font_size = px(size);
        });
    }
}

/// A panic aborts the release build, so its message would only reach the
/// terminal that launched the app, if any. Keep it in
/// ~/Library/Logs/Sense/panic.log too, with a backtrace.
fn install_panic_log() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(home) = std::env::var_os("HOME") {
            let dir = std::path::Path::new(&home).join("Library/Logs/Sense");
            let _ = std::fs::create_dir_all(&dir);
            let backtrace = std::backtrace::Backtrace::force_capture();
            let when = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default();
            let entry = format!("--- {when}\n{info}\n{backtrace}\n");
            if let Ok(mut log) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("panic.log"))
            {
                let _ = std::io::Write::write_all(&mut log, entry.as_bytes());
            }
        }
        default(info);
    }));
}
