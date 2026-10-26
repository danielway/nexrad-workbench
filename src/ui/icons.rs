//! The Phosphor icons the app draws, as the characters of the bundled
//! subset font `assets/fonts/Phosphor-Regular.ttf` (MIT, see
//! `Phosphor-LICENSE.txt` beside it; registered in
//! [`super::theme::font_definitions`]).
//!
//! The font holds only these glyphs. To add an icon, take its codepoint from
//! Phosphor's regular weight, add it here, and subset the font again with
//! every codepoint below, for example:
//!
//! ```text
//! pyftsubset Phosphor.ttf --unicodes=U+E094,U+E036,... \
//!     --output-file=assets/fonts/Phosphor-Regular.ttf \
//!     --layout-features='' --no-hinting --desubroutinize
//! ```

pub(crate) const ARROWS_CLOCKWISE: &str = "\u{E094}";
pub(crate) const ARROW_CLOCKWISE: &str = "\u{E036}";
pub(crate) const ARROW_DOWN: &str = "\u{E03E}";
pub(crate) const ARROW_LEFT: &str = "\u{E058}";
pub(crate) const ARROW_LINE_LEFT: &str = "\u{E062}";
pub(crate) const ARROW_LINE_RIGHT: &str = "\u{E064}";
pub(crate) const ARROW_RIGHT: &str = "\u{E06C}";
pub(crate) const ARROW_SQUARE_OUT: &str = "\u{E5DE}";
pub(crate) const BELL_SIMPLE: &str = "\u{E0D0}";
pub(crate) const BELL_SIMPLE_SLASH: &str = "\u{E0D2}";
pub(crate) const BOOKMARK_SIMPLE: &str = "\u{E0EA}";
pub(crate) const BROADCAST: &str = "\u{E0F2}";
pub(crate) const CALENDAR_BLANK: &str = "\u{E10A}";
pub(crate) const CARET_DOWN: &str = "\u{E136}";
pub(crate) const CARET_LEFT: &str = "\u{E138}";
pub(crate) const CARET_RIGHT: &str = "\u{E13A}";
pub(crate) const CARET_UP: &str = "\u{E13C}";
pub(crate) const CHECK_CIRCLE: &str = "\u{E184}";
pub(crate) const CHECK_SQUARE: &str = "\u{E186}";
pub(crate) const CIRCLE: &str = "\u{E18A}";
pub(crate) const CIRCLE_DASHED: &str = "\u{E602}";
pub(crate) const CIRCLE_HALF: &str = "\u{E18C}";
pub(crate) const CLOCK: &str = "\u{E19A}";
pub(crate) const CROSSHAIR: &str = "\u{E1D6}";
pub(crate) const DOTS_THREE: &str = "\u{E1FE}";
pub(crate) const DOWNLOAD_SIMPLE: &str = "\u{E20C}";
pub(crate) const EYE: &str = "\u{E220}";
pub(crate) const GEAR: &str = "\u{E270}";
pub(crate) const KEYBOARD: &str = "\u{E2D8}";
pub(crate) const LIST: &str = "\u{E2F0}";
pub(crate) const MAGNIFYING_GLASS: &str = "\u{E30C}";
pub(crate) const MAP_PIN: &str = "\u{E316}";
pub(crate) const MAP_PIN_LINE: &str = "\u{E318}";
pub(crate) const MAP_TRIFOLD: &str = "\u{E31A}";
pub(crate) const MINUS_CIRCLE: &str = "\u{E32C}";
pub(crate) const NAVIGATION_ARROW: &str = "\u{EADE}";
pub(crate) const PAUSE: &str = "\u{E39E}";
pub(crate) const PENCIL_SIMPLE: &str = "\u{E3B4}";
pub(crate) const PLAY: &str = "\u{E3D0}";
pub(crate) const QUESTION: &str = "\u{E3E8}";
pub(crate) const RADIO_BUTTON: &str = "\u{EB08}";
pub(crate) const RECORD: &str = "\u{E3EE}";
pub(crate) const RECTANGLE: &str = "\u{E3F0}";
pub(crate) const REPEAT: &str = "\u{E3F6}";
pub(crate) const RULER: &str = "\u{E6B8}";
pub(crate) const SIDEBAR_SIMPLE: &str = "\u{EC24}";
pub(crate) const SKIP_BACK: &str = "\u{E5A4}";
pub(crate) const SKIP_FORWARD: &str = "\u{E5A6}";
pub(crate) const SLIDERS_HORIZONTAL: &str = "\u{E434}";
pub(crate) const SPINNER: &str = "\u{E66A}";
pub(crate) const STACK: &str = "\u{E466}";
pub(crate) const STOP: &str = "\u{E46C}";
pub(crate) const TRASH: &str = "\u{E4A6}";
pub(crate) const WARNING: &str = "\u{E4E0}";
pub(crate) const X: &str = "\u{E4F6}";
