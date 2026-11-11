//! The journal: the app's always-on, append-only log of every state change, the system of
//! record a run can be rebuilt from. Ports `Journal/{JournalEvent, JournalState, Journal,
//! JournalReplay, JournalRewind, Notes}.swift` and the store values of
//! `Session/{FavoritesStore, VoiceVariationStore}.swift`.
//!
//! One JSONL file per app run; line 0 is a checkpoint ([`state::JournalState`]), every line
//! after is one event with its sequence number, wall-clock time (metadata only) and engine
//! frame (the timebase): `{"v":1,"seq","t","f","k",…}`. Mid-file checkpoints keep the decks'
//! playheads and leave the stores out. Readers drop a torn last line.
//!
//! This module is the pure model: codecs, the recorder's bookkeeping, replay planning and
//! the store edit functions. Writing the file, fsync and the directory are
//! `crate::host::journal`; standing a live engine on a state is the engine's.

pub mod event;
pub mod file;
pub mod replay;
pub mod state;
pub mod stores;
pub mod time;

pub use event::{JournalEntry, JournalEvent};
pub use file::{parse, Contents, JournalRecorder};
pub use replay::JournalError;
pub use state::JournalState;
pub use stores::{
    Favorite, JournalStoreState, NotePosition, NoteTarget, TrackNote, VoiceVariation,
};
