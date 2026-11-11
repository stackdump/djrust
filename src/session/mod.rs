//! A performance as data: sessions, their recorder and player, a written-down mix, the
//! CDN bundle a session publishes as, and WAV. Ports `Session/{SessionModel,
//! SessionRecorder, SessionPlayer, SessionMix, SessionPublish, ShareSource, SpecRender}.swift`
//! (the pure halves; sockets, files and ssh are `djhost`).

pub mod action;
pub mod arrange_start;
pub mod arrangement;
pub mod edits;
pub mod kits;
pub mod mix;
pub mod model;
pub mod player;
pub mod publish;
pub mod recorder;
pub mod source;
pub mod types;
pub mod wav;

pub use action::DeckAction;
pub use model::{DeckParams, DeckSource, Session, SessionError, SessionEvent, SessionEventKind};
