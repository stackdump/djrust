//! Gameplay fires Hits: the engine half of the `minecraftEvents` control method
//! (`AppControl.controlMinecraftEvents`, `MinecraftEvents.swift`).
//!
//! The `minecraft` MCP server's `mc_recent_events` (a ring buffer of `{at, source, event,
//! params}`, oldest first) is polled by whoever runs the loop (`djctl minecraft events` reads
//! the window on stdin) and handed to the host, which maps each new event through
//! `voices/minecraft-events.json` and performs it on a deck ([`Engine::perform`], so it records
//! as a button would). The pure half (the events, the map, the feed with its cursor and its
//! one-stinger-a-bar rule, the request and the reply) is `crate::deck::minecraft`; a mapped
//! stinger is `DeckAction::Stinger`, which is [`Engine::stinger`]: the deck's curated Hit,
//! including a Hit's loop on the loop row. No network and no game: the window is data.

use crate::deck::minecraft::{MinecraftEvent, MinecraftEventFeed};
use crate::json::JsonValue;
use crate::session::action::DeckAction;

use super::engine::Engine;
use super::perform::PerformCtx;

impl Engine {
    /// `controlMinecraftEvents`: `p` is `{events, deck?, prime?}`; the new events of the window
    /// become the actions the map names, on `deck` (default: the focus deck), each event once
    /// however often the window is re-read and a stinger at most once a bar. `feed` is the
    /// poller's memory and outlives the call. The reply is [`MinecraftEventFeed::report`]:
    /// what fired, what did not and why, and the cursor.
    pub fn minecraft_events(
        &mut self,
        feed: &mut MinecraftEventFeed,
        p: &JsonValue,
    ) -> Result<JsonValue, String> {
        let list = p
            .get("events")
            .ok_or("`events` must be mc_recent_events' array (or its result)")?;
        let events = MinecraftEvent::parse_json(list).map_err(|e| e.to_string())?;
        let deck = p
            .get("deck")
            .and_then(JsonValue::as_str)
            .map_or_else(|| self.focus.clone(), String::from);
        // `engine[name]`: the decks and the loop channel
        if self.deck(&deck).is_none() && deck != "L" {
            return Err(format!(
                "no deck \"{deck}\" (decks: {})",
                {
                    let mut names: Vec<&str> = self.decks.iter().map(|d| d.name.as_str()).collect();
                    names.sort_unstable();
                    names.join(", ")
                }
            ));
        }
        let bar = self.beat_state(&deck).map(|b| b.bar());
        let net = self.deck(&deck).is_some_and(|d| d.is_net());
        let prime = p.get("prime").and_then(JsonValue::as_bool).unwrap_or(false);
        let (fired, skipped) = feed.take(&events, &deck, bar, net, prime);
        for f in &fired {
            if let Some(a) = DeckAction::from_json(&f.action.json()) {
                self.perform(&a, PerformCtx::default());
            }
        }
        Ok(MinecraftEventFeed::report(&deck, &fired, &skipped, feed.cursor()))
    }
}
