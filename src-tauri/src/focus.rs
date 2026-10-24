//! Focus timer: pomodoro and longer modes. It runs here rather than in the page
//! because a desktop widget covered by other windows counts as hidden, and
//! Chromium then delays page timers by up to a minute.

use crate::desktop::APP_ID;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, sync::Mutex, thread, time::Duration};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_winrt_notification::Toast;

pub const LABEL: &str = "focus";
const MIN: i64 = 60_000;
/// A phase that ended longer ago than this (asleep, lid closed) passes quietly.
const ANNOUNCE_GRACE: i64 = 2 * MIN;
const MODES: [&str; 3] = ["pomodoro", "focus50", "deep90"];

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    #[default]
    Idle,
    Focus,
    Break,
    Long,
}

/// Focus, break and long-break lengths, and how many focus sessions earn a
/// long break (0 = never).
fn lengths(mode: &str) -> (i64, i64, i64, u32) {
    match mode {
        "focus50" => (50 * MIN, 10 * MIN, 10 * MIN, 0),
        "deep90" => (90 * MIN, 20 * MIN, 20 * MIN, 0),
        _ => (25 * MIN, 5 * MIN, 15 * MIN, 4),
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Timer {
    pub mode: String,
    pub phase: Phase,
    pub running: bool,
    /// While running: when the phase ends (unix ms).
    pub ends_at: i64,
    /// While not running: what's left of the phase.
    pub remaining: i64,
    /// Length of the current phase.
    pub total: i64,
    /// Focus sessions finished since the last long break.
    pub round: u32,
    /// Local date `today` counts for.
    pub day: String,
    /// Focus time finished that day, ms.
    pub today: i64,
    /// Chosen focus sound (`None` = silence) and its volume, 0..1.
    pub sound: Option<String>,
    pub volume: f64,
    /// Bumped on every change, so the page can drop a state that arrives late.
    pub rev: u64,
}

impl Default for Timer {
    fn default() -> Self {
        let mut t = Timer {
            mode: MODES[0].into(),
            phase: Phase::Idle,
            running: false,
            ends_at: 0,
            remaining: 0,
            total: 0,
            round: 0,
            day: String::new(),
            today: 0,
            sound: Some("rain".into()),
            volume: 0.6,
            rev: 0,
        };
        t.idle();
        t
    }
}

/// What just ended, for the chime and the notification.
#[derive(Debug, PartialEq)]
pub enum Done {
    Focus { long: bool, break_ms: i64 },
    Break,
}

impl Timer {
    /// Ready for the next focus session of the current mode.
    fn idle(&mut self) {
        let (focus, ..) = lengths(&self.mode);
        self.phase = Phase::Idle;
        self.running = false;
        self.total = focus;
        self.remaining = focus;
        self.ends_at = 0;
    }

    fn left(&self, now: i64) -> i64 {
        if self.running {
            (self.ends_at - now).clamp(0, self.total)
        } else {
            self.remaining
        }
    }

    /// A new day starts the count and the pomodoro cycle over.
    fn roll_day(&mut self, day: &str) {
        if self.day != day {
            self.day = day.to_string();
            self.today = 0;
            self.round = 0;
        }
    }

    /// The clock went back: a running phase can't have more than its length left.
    fn fix_clock(&mut self, now: i64) {
        if self.running && self.ends_at - now > self.total {
            self.ends_at = now + self.total;
        }
    }

    /// Starts a focus session, or resumes a paused phase.
    pub fn start(&mut self, now: i64) {
        if self.running {
            return;
        }
        if self.phase == Phase::Idle {
            self.phase = Phase::Focus;
        }
        self.ends_at = now + self.remaining;
        self.running = true;
    }

    pub fn pause(&mut self, now: i64) {
        if self.running {
            self.remaining = self.left(now);
            self.running = false;
        }
    }

    /// Ends the current phase early: a focus session keeps the time spent and
    /// goes on to its break (it counts as a round only if at least half was
    /// done); a break ends.
    pub fn skip(&mut self, now: i64) -> Option<Done> {
        match self.phase {
            Phase::Idle => None,
            Phase::Focus => {
                let spent = self.total - self.left(now);
                Some(self.finish_focus(now, spent, spent * 2 >= self.total))
            }
            Phase::Break | Phase::Long => {
                self.idle();
                Some(Done::Break)
            }
        }
    }

    /// Back to the start of a fresh cycle.
    pub fn reset(&mut self) {
        self.round = 0;
        self.idle();
    }

    pub fn set_mode(&mut self, mode: &str) -> bool {
        if !MODES.contains(&mode) {
            return false;
        }
        self.mode = mode.to_string();
        self.reset();
        true
    }

    /// Counts `spent` and starts the break that follows, from `from`.
    fn finish_focus(&mut self, from: i64, spent: i64, counts: bool) -> Done {
        self.today += spent.max(0);
        let (_, short, long, every) = lengths(&self.mode);
        if counts {
            self.round += 1;
        }
        let is_long = every > 0 && self.round >= every;
        if is_long {
            self.round = 0;
        }
        self.phase = if is_long { Phase::Long } else { Phase::Break };
        self.total = if is_long { long } else { short };
        self.remaining = self.total;
        self.ends_at = from + self.total;
        self.running = true;
        Done::Focus { long: is_long, break_ms: self.total }
    }

    /// Moves a running timer past a phase that has ended (one phase per call).
    /// A focus session flows straight into its break; after a break the timer
    /// waits for the user.
    pub fn tick(&mut self, now: i64) -> Option<Done> {
        self.fix_clock(now);
        if !self.running || now < self.ends_at {
            return None;
        }
        match self.phase {
            Phase::Focus => {
                let (end, total) = (self.ends_at, self.total);
                Some(self.finish_focus(end, total, true))
            }
            Phase::Break | Phase::Long => {
                self.idle();
                Some(Done::Break)
            }
            Phase::Idle => {
                self.running = false;
                None
            }
        }
    }

    /// Repairs a state read from disk (older version, hand edits).
    fn sanitize(&mut self) {
        if !MODES.contains(&self.mode.as_str()) {
            self.mode = MODES[0].into();
            self.reset();
        }
        self.volume = if self.volume.is_finite() { self.volume.clamp(0.0, 1.0) } else { 0.6 };
        if self.total <= 0 || self.remaining < 0 || self.remaining > self.total {
            self.idle();
        }
        self.fix_clock(crate::store::now_ms());
    }
}

pub struct Focus {
    path: PathBuf,
    timer: Mutex<Timer>,
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

impl Focus {
    pub fn load(path: PathBuf) -> Self {
        let mut timer: Timer = fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        timer.sanitize();
        // Phases that ended while the app was closed pass quietly.
        let now = crate::store::now_ms();
        while timer.tick(now).is_some() {}
        Focus { path, timer: Mutex::new(timer) }
    }

    fn save(&self, timer: &Timer) {
        let Ok(json) = serde_json::to_string_pretty(timer) else { return };
        let tmp = self.path.with_extension("json.tmp");
        if fs::write(&tmp, json).is_ok() {
            let _ = fs::rename(&tmp, &self.path);
        }
    }
}

/// Applies `f` to the timer, saves it and tells the focus widget.
fn change<R>(app: &AppHandle, f: impl FnOnce(&mut Timer, i64) -> R) -> (R, Timer) {
    let focus = app.state::<Focus>();
    let mut timer = focus.timer.lock().unwrap();
    timer.roll_day(&today());
    let out = f(&mut timer, crate::store::now_ms());
    timer.rev += 1;
    focus.save(&timer);
    let snapshot = timer.clone();
    drop(timer);
    let _ = app.emit_to(LABEL, "focus", &snapshot);
    (out, snapshot)
}

fn announce(app: &AppHandle, done: &Done) {
    let (chime, title, text) = match done {
        Done::Focus { long, break_ms } => (
            "focus-end",
            "Фокус завершён",
            format!("{} {} мин", if *long { "Длинный перерыв" } else { "Перерыв" }, break_ms / MIN),
        ),
        Done::Break => ("break-end", "Перерыв окончен", "Можно возвращаться к фокусу".to_string()),
    };
    let _ = app.emit_to(LABEL, "focus-chime", chime);
    let _ = Toast::new(APP_ID).title(title).text1(&text).show();
}

pub fn spawn(app: AppHandle) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(250));
        let focus = app.state::<Focus>();
        // A phase ended, or the clock went back past its start (see fix_clock).
        let due = {
            let t = focus.timer.lock().unwrap();
            let now = crate::store::now_ms();
            t.running && (now >= t.ends_at || t.ends_at - now > t.total)
        };
        if due {
            // After sleep several phases may have ended: pass them all and
            // announce only the last, and only if it ended just now.
            let (last, _) = change(&app, |t, now| {
                let mut last = None;
                loop {
                    let ended_at = t.ends_at;
                    match t.tick(now) {
                        Some(done) => last = Some((done, now - ended_at)),
                        None => break last,
                    }
                }
            });
            if let Some((done, late)) = last {
                if late <= ANNOUNCE_GRACE {
                    announce(&app, &done);
                }
            }
        }
    });
}

#[tauri::command]
pub fn focus_state(focus: State<Focus>) -> Timer {
    focus.timer.lock().unwrap().clone()
}

/// `start`, `pause`, `skip` or `reset`. Skipping is the user's own doing, so it
/// isn't announced.
#[tauri::command]
pub fn focus_action(app: AppHandle, action: String) -> Result<Timer, String> {
    let (ok, timer) = change(&app, |t, now| match action.as_str() {
        "start" => {
            t.start(now);
            true
        }
        "pause" => {
            t.pause(now);
            true
        }
        "skip" => {
            t.skip(now);
            true
        }
        "reset" => {
            t.reset();
            true
        }
        _ => false,
    });
    if ok { Ok(timer) } else { Err(format!("unknown action {action}")) }
}

#[tauri::command]
pub fn focus_mode(app: AppHandle, mode: String) -> Result<Timer, String> {
    let (ok, timer) = change(&app, |t, _| t.set_mode(&mode));
    if ok { Ok(timer) } else { Err(format!("unknown mode {mode}")) }
}

/// Remembers the chosen sound (`None` = silence) and volume.
#[tauri::command]
pub fn focus_prefs(app: AppHandle, sound: Option<String>, volume: f64) -> Timer {
    let sound = sound.filter(|s| !s.is_empty() && s.len() <= 32);
    change(&app, |t, _| {
        t.sound = sound;
        t.volume = if volume.is_finite() { volume.clamp(0.0, 1.0) } else { t.volume };
    })
    .1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timer(mode: &str) -> Timer {
        let mut t = Timer::default();
        t.set_mode(mode);
        t
    }

    #[test]
    fn pomodoro_cycle_with_long_break_every_fourth() {
        let mut t = timer("pomodoro");
        let mut now = 0;
        for round in 1..=4 {
            t.start(now);
            assert_eq!(t.phase, Phase::Focus);
            now = t.ends_at;
            let done = t.tick(now).unwrap();
            let long = round == 4;
            assert_eq!(done, Done::Focus { long, break_ms: if long { 15 * MIN } else { 5 * MIN } });
            assert_eq!(t.phase, if long { Phase::Long } else { Phase::Break });
            assert!(t.running, "the break starts by itself");
            now = t.ends_at;
            assert_eq!(t.tick(now), Some(Done::Break));
            assert_eq!(t.phase, Phase::Idle);
            assert!(!t.running, "the next focus waits for the user");
        }
        assert_eq!(t.today, 4 * 25 * MIN);
        assert_eq!(t.round, 0);
    }

    #[test]
    fn long_modes_never_take_a_long_break() {
        let mut t = timer("deep90");
        for _ in 0..5 {
            t.start(0);
            assert_eq!(t.tick(t.ends_at), Some(Done::Focus { long: false, break_ms: 20 * MIN }));
            t.tick(t.ends_at);
        }
        assert_eq!(t.total, 90 * MIN);
    }

    #[test]
    fn pause_keeps_the_rest_and_resume_continues() {
        let mut t = timer("focus50");
        t.start(1_000);
        t.pause(1_000 + 10 * MIN);
        assert!(!t.running);
        assert_eq!(t.remaining, 40 * MIN);
        assert_eq!(t.tick(i64::MAX), None, "a paused timer never ends");
        t.start(5 * 60 * MIN);
        assert_eq!(t.ends_at, 5 * 60 * MIN + 40 * MIN);
    }

    #[test]
    fn skipping_focus_counts_the_time_spent() {
        let mut t = timer("pomodoro");
        t.start(0);
        assert_eq!(t.skip(10 * MIN), Some(Done::Focus { long: false, break_ms: 5 * MIN }));
        assert_eq!(t.today, 10 * MIN);
        assert_eq!(t.round, 0, "less than half done isn't a round");
        assert_eq!(t.phase, Phase::Break);
        assert_eq!(t.skip(11 * MIN), Some(Done::Break));
        assert_eq!(t.phase, Phase::Idle);
        assert_eq!(t.skip(12 * MIN), None);
        t.start(20 * MIN);
        t.skip(20 * MIN + 13 * MIN);
        assert_eq!(t.round, 1, "past half it is");
    }

    #[test]
    fn a_clock_set_back_cant_stretch_a_phase() {
        let mut t = timer("pomodoro");
        t.start(10 * 60 * MIN);
        assert_eq!(t.tick(9 * 60 * MIN), None);
        assert_eq!(t.ends_at, 9 * 60 * MIN + 25 * MIN);
    }

    #[test]
    fn phases_missed_while_closed_pass_in_order() {
        let mut t = timer("pomodoro");
        t.start(0);
        let mut steps = 0;
        while t.tick(3 * 60 * MIN).is_some() {
            steps += 1;
        }
        assert_eq!(steps, 2);
        assert_eq!(t.phase, Phase::Idle);
        assert_eq!(t.today, 25 * MIN);
    }

    #[test]
    fn mode_change_and_reset_start_over() {
        let mut t = timer("pomodoro");
        t.start(0);
        t.tick(t.ends_at);
        assert!(t.set_mode("deep90"));
        assert_eq!((t.phase, t.round, t.total, t.running), (Phase::Idle, 0, 90 * MIN, false));
        assert!(!t.set_mode("nonsense"));
        assert_eq!(t.mode, "deep90");
    }

    #[test]
    fn new_day_starts_the_count_over() {
        let mut t = timer("pomodoro");
        t.roll_day("2026-10-24");
        t.today = 50 * MIN;
        t.round = 2;
        t.roll_day("2026-10-24");
        assert_eq!((t.today, t.round), (50 * MIN, 2));
        t.roll_day("2026-10-25");
        assert_eq!((t.today, t.round), (0, 0));
    }

    #[test]
    fn bad_saved_state_is_repaired() {
        let mut t: Timer = serde_json::from_str(r#"{"mode":"x","remaining":-5,"volume":7}"#).unwrap();
        t.sanitize();
        assert_eq!((t.mode.as_str(), t.phase, t.remaining, t.volume), ("pomodoro", Phase::Idle, 25 * MIN, 1.0));
    }
}
