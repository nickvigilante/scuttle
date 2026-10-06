//! The user's compaction threshold for each model: what the server holds, the edit on the
//! highlighted `/model` row, and the one save in flight for each model.
//!
//! The server compacts a chat once its context reaches this percent of the model's window.
//! The user's override wins over the model's `compression_threshold`; 100 never compacts, and
//! 0 compacts after every turn that reports context usage (`shouldCompactPromptUsage` in
//! `coderd/x/chatd/generation_preparer.go`).

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

/// How far Left or Right moves a threshold, in percent.
pub const STEP: i64 = 5;

/// What a save asks of the server for one model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Store this percent as the user's override, with `PUT`.
    Set(i64),
    /// Remove the override, so the model's default applies, with `DELETE`.
    Reset,
}

/// A save for the runtime to send, answered by `Msg::ThresholdSaved` or
/// `Msg::ThresholdFailed` with its `generation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Save {
    pub model: Uuid,
    pub change: Change,
    pub generation: u64,
}

/// Whether the user's overrides have loaded.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum State {
    #[default]
    Loading,
    Loaded,
    /// The first load failed, with why; the next edit loads them again.
    Failed(String),
}

/// A model's threshold as the `/model` table shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    /// The overrides are still loading.
    Loading,
    /// The overrides failed to load, or the model names no default, so the threshold in
    /// effect is not known.
    Unknown,
    /// The threshold in effect, and whether it is the model's default rather than the user's
    /// override.
    Known { percent: i64, default: bool },
}

/// The threshold one step from `percent`, `up` or down: the next multiple of [`STEP`] that
/// way, within 0 and 100, so a value set elsewhere, such as 72, joins the grid.
pub fn next_step(percent: i64, up: bool) -> i64 {
    let next = if up {
        (percent.div_euclid(STEP) + 1) * STEP
    } else {
        ((percent + STEP - 1).div_euclid(STEP) - 1) * STEP
    };
    next.clamp(0, 100)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InFlight {
    generation: u64,
    change: Change,
}

/// The user's overrides and the edits to them. The core owns it, so the request generations
/// stay in the core, as every other request's do.
#[derive(Debug, Default)]
pub struct Thresholds {
    state: State,
    /// The overrides the server last confirmed, by model config id.
    confirmed: HashMap<Uuid, i64>,
    /// The edit on the highlighted row, sent once the row is left or the table closes.
    draft: Option<(Uuid, Change)>,
    /// The one save in flight for each model, so two saves for one model never race.
    in_flight: HashMap<Uuid, InFlight>,
    /// The newest edit made while a save for the same model was in flight, sent after it.
    queued: HashMap<Uuid, Change>,
    /// The generation of the newest request of any kind; each request takes the next.
    generation: u64,
    /// The load in flight, by generation.
    loading: Option<u64>,
    /// The models with a save in flight at some time while the load was, whose value the
    /// load's reply may have read before the save landed.
    touched: HashSet<Uuid>,
}

impl Thresholds {
    pub fn state(&self) -> &State {
        &self.state
    }

    /// Starts a load of the overrides and returns its generation, for
    /// `Effect::FetchThresholds`. Values already loaded keep showing until the reply.
    pub fn start_load(&mut self) -> u64 {
        self.generation += 1;
        self.loading = Some(self.generation);
        self.touched = self.in_flight.keys().copied().collect();
        if self.state != State::Loaded {
            self.state = State::Loading;
        }
        self.generation
    }

    /// Applies the overrides the load numbered `generation` read. A reply to an older load
    /// is dropped, and a model whose save was in flight during the load keeps the value its
    /// save confirmed.
    pub fn loaded(&mut self, overrides: Vec<(Uuid, i64)>, generation: u64) {
        if self.loading != Some(generation) {
            return;
        }
        self.loading = None;
        let touched = std::mem::take(&mut self.touched);
        let mut confirmed: HashMap<Uuid, i64> = overrides
            .into_iter()
            .filter(|(m, _)| !touched.contains(m))
            .collect();
        for m in &touched {
            if let Some(percent) = self.confirmed.get(m) {
                confirmed.insert(*m, *percent);
            }
        }
        self.confirmed = confirmed;
        self.state = State::Loaded;
    }

    /// Records that the load numbered `generation` failed. Values already loaded stay.
    pub fn load_failed(&mut self, message: String, generation: u64) {
        if self.loading != Some(generation) {
            return;
        }
        self.loading = None;
        self.touched.clear();
        if self.state != State::Loaded {
            self.state = State::Failed(message);
        }
    }

    /// `model`'s threshold as `/model` shows it: the edit on its row, else the edit waiting or
    /// in flight, else what the server confirmed, else `default`, the model's own.
    pub fn shown(&self, model: Uuid, default: Option<i64>) -> Shown {
        match self.state {
            State::Loading => return Shown::Loading,
            State::Failed(_) => return Shown::Unknown,
            State::Loaded => {}
        }
        let pending = self
            .draft
            .filter(|(m, _)| *m == model)
            .map(|(_, change)| change)
            .or_else(|| self.queued.get(&model).copied())
            .or_else(|| self.in_flight.get(&model).map(|f| f.change));
        let chosen = match pending {
            Some(Change::Set(percent)) => Some(percent),
            Some(Change::Reset) => None,
            None => self.confirmed.get(&model).copied(),
        };
        match (chosen, default) {
            (Some(percent), _) => Shown::Known {
                percent,
                default: false,
            },
            (None, Some(percent)) => Shown::Known {
                percent,
                default: true,
            },
            (None, None) => Shown::Unknown,
        }
    }

    /// Moves `model`'s threshold one step as an edit that is shown but not sent. An edit left
    /// on another model is sent first, and returned.
    pub fn step(&mut self, model: Uuid, up: bool, default: Option<i64>) -> Option<Save> {
        let other = self.commit_other(model);
        if let Shown::Known { percent, .. } = self.shown(model, default) {
            let next = next_step(percent, up);
            if next != percent {
                self.draft = Some((model, Change::Set(next)));
            }
        }
        other
    }

    /// Goes back to `model`'s default as an edit, sent as a step is.
    pub fn reset(&mut self, model: Uuid) -> Option<Save> {
        let other = self.commit_other(model);
        if self.state == State::Loaded {
            self.draft = Some((model, Change::Reset));
        }
        other
    }

    fn commit_other(&mut self, model: Uuid) -> Option<Save> {
        if self.draft.is_some_and(|(m, _)| m != model) {
            self.commit()
        } else {
            None
        }
    }

    /// Sends the edit on the highlighted row, unless the server already holds it. While a save
    /// for the same model is in flight, the edit waits for that save's reply.
    pub fn commit(&mut self) -> Option<Save> {
        let (model, change) = self.draft.take()?;
        if self.in_flight.contains_key(&model) {
            self.queued.insert(model, change);
            return None;
        }
        self.send(model, change)
    }

    /// Applies the reply to the save numbered `generation`: the override the server now holds,
    /// or `None` after a reset. A stale reply is dropped. The edit that waited, if any, is sent
    /// next and returned.
    pub fn saved(&mut self, model: Uuid, percent: Option<i64>, generation: u64) -> Option<Save> {
        if !self.is_current(model, generation) {
            return None;
        }
        self.in_flight.remove(&model);
        match percent {
            Some(percent) => {
                self.confirmed.insert(model, percent);
            }
            None => {
                self.confirmed.remove(&model);
            }
        }
        let next = self.queued.remove(&model)?;
        self.send(model, next)
    }

    /// Applies a refusal of the save numbered `generation`: the row goes back to what the
    /// server confirmed, and the edits made since are dropped. Returns whether the reply was
    /// current, so the caller reports only that one.
    pub fn failed(&mut self, model: Uuid, generation: u64) -> bool {
        if !self.is_current(model, generation) {
            return false;
        }
        self.in_flight.remove(&model);
        self.queued.remove(&model);
        if self.draft.is_some_and(|(m, _)| m == model) {
            self.draft = None;
        }
        true
    }

    fn is_current(&self, model: Uuid, generation: u64) -> bool {
        self.in_flight
            .get(&model)
            .is_some_and(|f| f.generation == generation)
    }

    fn send(&mut self, model: Uuid, change: Change) -> Option<Save> {
        let held = self.confirmed.get(&model).copied();
        let unchanged = match change {
            Change::Set(percent) => held == Some(percent),
            Change::Reset => held.is_none(),
        };
        if unchanged {
            return None;
        }
        self.generation += 1;
        let generation = self.generation;
        self.in_flight
            .insert(model, InFlight { generation, change });
        if self.loading.is_some() {
            self.touched.insert(model);
        }
        Some(Save {
            model,
            change,
            generation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(overrides: Vec<(Uuid, i64)>) -> Thresholds {
        let mut t = Thresholds::default();
        let generation = t.start_load();
        t.loaded(overrides, generation);
        t
    }

    #[test]
    fn a_step_moves_to_the_next_five_within_0_and_100() {
        assert_eq!(next_step(70, true), 75);
        assert_eq!(next_step(72, true), 75);
        assert_eq!(next_step(72, false), 70);
        assert_eq!(next_step(70, false), 65);
        assert_eq!(next_step(98, true), 100);
        assert_eq!(next_step(100, true), 100);
        assert_eq!(next_step(3, false), 0);
        assert_eq!(next_step(0, false), 0);
    }

    #[test]
    fn stepping_past_100_or_below_0_clamps_and_an_unmoved_step_makes_no_edit() {
        let m = Uuid::new_v4();
        let mut t = loaded(vec![]);
        for _ in 0..10 {
            assert_eq!(t.step(m, true, Some(70)), None, "nothing is sent per step");
        }
        assert_eq!(
            t.shown(m, Some(70)),
            Shown::Known {
                percent: 100,
                default: false
            }
        );
        for _ in 0..30 {
            t.step(m, false, Some(70));
        }
        assert_eq!(
            t.shown(m, Some(70)),
            Shown::Known {
                percent: 0,
                default: false
            }
        );
        let mut at_top = loaded(vec![]);
        at_top.step(m, true, Some(100));
        assert_eq!(
            at_top.commit(),
            None,
            "Right on a default of 100 sets no override"
        );
    }

    #[test]
    fn a_reset_deletes_only_an_override_the_server_holds() {
        let (set, unset) = (Uuid::new_v4(), Uuid::new_v4());
        let mut t = loaded(vec![(set, 50)]);
        t.reset(unset);
        assert_eq!(t.commit(), None, "no override to delete");
        t.reset(set);
        assert_eq!(
            t.shown(set, Some(30)),
            Shown::Known {
                percent: 30,
                default: true
            }
        );
        let save = t.commit().expect("a delete");
        assert_eq!((save.model, save.change), (set, Change::Reset));
        assert_eq!(t.saved(set, None, save.generation), None);
        assert_eq!(
            t.shown(set, Some(30)),
            Shown::Known {
                percent: 30,
                default: true
            }
        );
    }

    #[test]
    fn an_edit_on_another_model_sends_the_one_left_behind() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut t = loaded(vec![]);
        assert_eq!(t.step(a, true, Some(70)), None);
        assert_eq!(t.step(a, true, Some(70)), None);
        let save = t
            .step(b, false, Some(30))
            .expect("leaving a sends its edit");
        assert_eq!((save.model, save.change), (a, Change::Set(80)));
        assert_eq!(
            t.shown(b, Some(30)),
            Shown::Known {
                percent: 25,
                default: false
            }
        );
    }

    #[test]
    fn a_failed_save_shows_the_server_value_and_drops_the_edit_that_waited() {
        let m = Uuid::new_v4();
        let mut t = loaded(vec![(m, 50)]);
        t.step(m, true, Some(70));
        let save = t.commit().expect("a save");
        t.step(m, true, Some(70));
        assert_eq!(t.commit(), None, "waits for the save in flight");
        assert_eq!(
            t.shown(m, Some(70)),
            Shown::Known {
                percent: 60,
                default: false
            }
        );
        assert!(
            !t.failed(m, save.generation + 1),
            "a stale failure is dropped"
        );
        assert!(t.failed(m, save.generation));
        assert_eq!(
            t.shown(m, Some(70)),
            Shown::Known {
                percent: 50,
                default: false
            }
        );
        assert!(!t.failed(m, save.generation), "a reply is applied once");
    }

    #[test]
    fn one_save_per_model_is_in_flight_and_a_stale_reply_is_dropped() {
        let m = Uuid::new_v4();
        let mut t = loaded(vec![]);
        t.step(m, true, Some(70));
        let first = t.commit().expect("a save");
        t.step(m, true, Some(70));
        assert_eq!(t.commit(), None);
        assert_eq!(t.saved(m, Some(40), first.generation + 100), None);
        assert_eq!(
            t.shown(m, Some(70)),
            Shown::Known {
                percent: 80,
                default: false
            },
            "a stale reply never overwrites the newer edit"
        );
        let next = t
            .saved(m, Some(75), first.generation)
            .expect("the edit that waited goes next");
        assert_eq!(next.change, Change::Set(80));
        assert!(next.generation > first.generation);
    }

    #[test]
    fn a_load_never_overwrites_a_save_made_while_it_was_in_flight() {
        let (saved, other) = (Uuid::new_v4(), Uuid::new_v4());
        let mut t = loaded(vec![]);
        let load = t.start_load();
        t.step(saved, true, Some(70));
        let save = t.commit().expect("a save");
        assert_eq!(t.saved(saved, Some(75), save.generation), None);
        // The load read the server before the save landed.
        t.loaded(vec![(other, 40)], load);
        assert_eq!(
            t.shown(saved, Some(70)),
            Shown::Known {
                percent: 75,
                default: false
            }
        );
        assert_eq!(
            t.shown(other, Some(70)),
            Shown::Known {
                percent: 40,
                default: false
            }
        );
        let older = t.start_load();
        let newer = t.start_load();
        t.loaded(vec![], older);
        assert_eq!(
            t.shown(other, Some(70)),
            Shown::Known {
                percent: 40,
                default: false
            },
            "an older load's reply is dropped"
        );
        t.loaded(vec![], newer);
        assert_eq!(
            t.shown(other, Some(70)),
            Shown::Known {
                percent: 70,
                default: true
            }
        );
    }

    #[test]
    fn a_failed_load_shows_unknown_and_keeps_values_already_loaded() {
        let m = Uuid::new_v4();
        let mut first = Thresholds::default();
        assert_eq!(first.shown(m, Some(70)), Shown::Loading);
        let generation = first.start_load();
        first.load_failed("HTTP 502".into(), generation);
        assert_eq!(first.state(), &State::Failed("HTTP 502".into()));
        assert_eq!(first.shown(m, Some(70)), Shown::Unknown);
        let mut later = loaded(vec![(m, 50)]);
        let generation = later.start_load();
        later.load_failed("HTTP 502".into(), generation);
        assert_eq!(later.state(), &State::Loaded);
        assert_eq!(
            later.shown(m, Some(70)),
            Shown::Known {
                percent: 50,
                default: false
            }
        );
    }
}
