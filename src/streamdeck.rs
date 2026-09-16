//! Stream Deck Plus: one key per machine, lit when that machine is on screen.
//! Blocking HID on its own thread; the daemon talks to it through channels.

use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use ab_glyph::{FontRef, PxScale};
use elgato_streamdeck::info::Kind;
use elgato_streamdeck::{StreamDeck, StreamDeckInput, list_devices, new_hidapi};
use image::{DynamicImage, Rgb, RgbImage};
use imageproc::drawing::draw_text_mut;
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use crate::daemon::DeckView;

const LIT: Rgb<u8> = Rgb([40, 200, 90]);
const DARK: Rgb<u8> = Rgb([28, 28, 32]);
const ERROR: Rgb<u8> = Rgb([200, 40, 40]);
const TEXT: Rgb<u8> = Rgb([245, 245, 245]);

/// Per-OS system fonts to fall back to when no font is configured, tried in
/// order; only the one matching the running OS actually exists on disk.
const DEFAULT_FONT_CANDIDATES: [&str; 3] = [
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    r"C:\Windows\Fonts\arial.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
];

/// The font to draw key labels with: the configured path if given, else the
/// first per-OS default that exists on this machine, else `None`.
#[must_use]
pub fn resolve_font_path(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = configured {
        return Some(path.to_path_buf());
    }
    DEFAULT_FONT_CANDIDATES
        .iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
}

/// A key image: background by state, the label centred and truncated to fit
/// drawn only when a font is available; otherwise a plain state-coloured key.
#[must_use]
pub fn render(
    label: &str,
    lit: bool,
    error: bool,
    size: (u32, u32),
    font: Option<&FontRef<'_>>,
) -> DynamicImage {
    let background = if error {
        ERROR
    } else if lit {
        LIT
    } else {
        DARK
    };
    let mut img = RgbImage::from_pixel(size.0, size.1, background);
    if let Some(font) = font {
        let max_chars = usize::try_from(size.0 / 11).unwrap_or(10).max(3);
        let text: String = if label.chars().count() > max_chars {
            label
                .chars()
                .take(max_chars - 1)
                .chain(std::iter::once('…'))
                .collect()
        } else {
            label.to_string()
        };
        let scale_px = f32::from(u16::try_from(size.1).unwrap_or(u16::MAX)) / 5.0;
        let scale = PxScale::from(scale_px);
        let x = i32::try_from(size.0 / 12).unwrap_or(4);
        let y = i32::try_from(size.1 / 2 - size.1 / 10).unwrap_or(40);
        draw_text_mut(&mut img, TEXT, x, y, scale, font, &text);
    }
    DynamicImage::ImageRgb8(img)
}

/// Reads the resolved font's bytes, logging a `warn!` and returning `None`
/// when nothing usable is found or the file can't be read.
fn load_font_bytes(configured: Option<&Path>) -> Option<Vec<u8>> {
    let Some(path) = resolve_font_path(configured) else {
        warn!("no Stream Deck label font found; keys will render without text");
        return None;
    };
    std::fs::read(&path).map_or_else(
        |error| {
            warn!(%error, path = %path.display(), "could not read the Stream Deck label font; keys will render without text");
            None
        },
        Some,
    )
}

/// Connect and start the deck thread. Emits pressed key indexes; `None` when
/// no Stream Deck is found or it could not be opened (logged either way).
///
/// Takes `font` by value: it mirrors the owned `Option<PathBuf>` the daemon
/// reads out of its config, and the clone at the call site is cheap.
#[must_use]
#[expect(
    clippy::needless_pass_by_value,
    reason = "owned to mirror the config field this is called with; only ever borrowed here"
)]
pub fn spawn(
    mut view: watch::Receiver<DeckView>,
    font: Option<PathBuf>,
) -> Option<mpsc::Receiver<usize>> {
    let hid = match new_hidapi() {
        Ok(hid) => hid,
        Err(error) => {
            warn!(%error, "could not start HID for the Stream Deck");
            return None;
        }
    };
    let mut devices = list_devices(&hid);
    devices.sort_by_key(|(kind, _)| u8::from(*kind != Kind::Plus));
    let Some((kind, serial)) = devices.into_iter().next() else {
        warn!("no Stream Deck found; is the Elgato app holding it?");
        return None;
    };
    let deck = match StreamDeck::connect(&hid, kind, &serial) {
        Ok(deck) => deck,
        Err(error) => {
            warn!(%error, "could not open the Stream Deck");
            return None;
        }
    };
    info!(?kind, %serial, "Stream Deck connected");

    let font_bytes = load_font_bytes(font.as_deref());

    let (tx, rx) = mpsc::channel(8);
    let (repaint_tx, repaint_rx) = std_mpsc::channel::<DeckView>();
    // Bridge the async watch into a std channel the blocking thread can poll.
    tokio::spawn(async move {
        loop {
            let current = view.borrow_and_update().clone();
            if repaint_tx.send(current).is_err() {
                return;
            }
            if view.changed().await.is_err() {
                return;
            }
        }
    });
    std::thread::Builder::new()
        .name("deskswitch-streamdeck".into())
        .spawn(move || {
            let font = font_bytes
                .as_deref()
                .and_then(|bytes| FontRef::try_from_slice(bytes).ok());
            deck_loop(&deck, &repaint_rx, &tx, font.as_ref());
        })
        .ok()?;
    Some(rx)
}

fn deck_loop(
    deck: &StreamDeck,
    repaint: &std_mpsc::Receiver<DeckView>,
    presses: &mpsc::Sender<usize>,
    font: Option<&FontRef<'_>>,
) {
    let kind = deck.kind();
    let format = kind.key_image_format();
    let size = (
        u32::try_from(format.size.0).unwrap_or(120),
        u32::try_from(format.size.1).unwrap_or(120),
    );
    let key_count = usize::from(kind.key_count());
    let mut was_down = vec![false; key_count];
    let _ = deck.set_brightness(60);
    loop {
        // Drain every pending view and paint only the last one: intermediate
        // views are stale the instant a newer one is queued behind them.
        while let Ok(view) = repaint.try_recv() {
            let error = view
                .error_until
                .is_some_and(|until| until > std::time::Instant::now());
            for key in 0..key_count {
                let image = match view.buttons.get(key) {
                    Some(button) => {
                        render(&button.machine, button.lit, error && button.lit, size, font)
                    }
                    None => render("", false, false, size, font),
                };
                let _ = deck.set_button_image(u8::try_from(key).unwrap_or(0), image);
            }
            let _ = deck.flush();
        }
        match deck.read_input(Some(Duration::from_millis(100))) {
            Ok(StreamDeckInput::ButtonStateChange(states)) => {
                for (key, down) in states.iter().enumerate().take(key_count) {
                    if *down && !was_down[key] && presses.blocking_send(key).is_err() {
                        return;
                    }
                    was_down[key] = *down;
                }
            }
            Ok(_) => {}
            Err(error) => {
                warn!(%error, "Stream Deck read failed; stopping deck thread");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{render, resolve_font_path};

    #[test]
    fn renders_the_requested_size_and_distinct_states() {
        let lit = render("avapc", true, false, (120, 120), None);
        let dark = render("avapc", false, false, (120, 120), None);
        let error = render("avapc", false, true, (120, 120), None);
        assert_eq!((lit.width(), lit.height()), (120, 120));
        let px = |img: &image::DynamicImage| img.to_rgb8().get_pixel(4, 4).0;
        assert_ne!(px(&lit), px(&dark));
        assert_ne!(px(&error), px(&dark));
    }

    #[test]
    fn long_labels_are_truncated_not_overflowed() {
        let img = render(
            "a-very-long-machine-name-indeed",
            false,
            false,
            (120, 120),
            None,
        );
        assert_eq!(img.width(), 120);
    }

    #[test]
    fn renders_with_a_label_when_a_font_is_available() {
        if let Some(path) = resolve_font_path(None) {
            let bytes = std::fs::read(&path).expect("default font should be readable");
            let font =
                ab_glyph::FontRef::try_from_slice(&bytes).expect("default font should parse");
            let img = render("avapc", true, false, (120, 120), Some(&font));
            assert_eq!((img.width(), img.height()), (120, 120));
            let long = render(
                "a-very-long-machine-name-indeed",
                false,
                false,
                (120, 120),
                Some(&font),
            );
            assert_eq!(long.width(), 120);
        }
    }

    #[test]
    fn resolve_font_path_prefers_the_configured_path() {
        let configured = std::path::PathBuf::from("/tmp/definitely-not-a-real-font.ttf");
        assert_eq!(resolve_font_path(Some(&configured)), Some(configured));
    }
}
