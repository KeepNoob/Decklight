use crate::app::AppCommand;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct PresentationState {
    current_index: usize,
    total_slides: usize,
    labels: Vec<String>,
    use_slide_labels: bool,
    pub screen_roles_swapped: bool,
    pub projector_fullscreen: bool,
    pub laser_enabled: bool,
    pub highlight_enabled: bool,
    auto_advance_enabled: bool,
    auto_advance_global_interval_secs: u32,
    auto_advance_slide_overrides_secs: BTreeMap<usize, u32>,
    last_auto_tick: Instant,
    started_at: Option<Instant>,
    current_slide_started_at: Instant,
    per_slide_elapsed: Vec<Duration>,
}

impl PresentationState {
    pub fn with_labels(total_slides: usize, labels: Vec<String>, now: Instant) -> Self {
        let total = total_slides.max(1);
        let mut normalized_labels = labels;
        if normalized_labels.len() != total {
            normalized_labels = (0..total).map(|i| (i + 1).to_string()).collect();
        }

        Self {
            current_index: 0,
            total_slides: total,
            labels: normalized_labels,
            use_slide_labels: false,
            screen_roles_swapped: false,
            projector_fullscreen: true,
            laser_enabled: false,
            highlight_enabled: false,
            auto_advance_enabled: false,
            auto_advance_global_interval_secs: 12,
            auto_advance_slide_overrides_secs: BTreeMap::new(),
            last_auto_tick: now,
            started_at: None,
            current_slide_started_at: now,
            per_slide_elapsed: vec![Duration::ZERO; total],
        }
    }

    pub fn current_index(&self) -> usize {
        self.current_index
    }

    pub fn current_label(&self) -> &str {
        &self.labels[self.current_index]
    }

    pub fn next_label(&self) -> Option<&str> {
        self.labels.get(self.current_index + 1).map(String::as_str)
    }

    pub fn label_at(&self, index: usize) -> Option<&str> {
        self.labels.get(index).map(String::as_str)
    }

    pub fn next_label_from(&self, index: usize) -> Option<&str> {
        self.labels.get(index + 1).map(String::as_str)
    }

    pub fn total_slides(&self) -> usize {
        self.total_slides
    }

    pub fn use_slide_labels(&self) -> bool {
        self.use_slide_labels
    }

    pub fn auto_advance_enabled(&self) -> bool {
        self.auto_advance_enabled
    }

    pub fn auto_advance_global_interval_secs(&self) -> u32 {
        self.auto_advance_global_interval_secs
    }

    pub fn auto_advance_slide_override_secs(&self, index: usize) -> Option<u32> {
        self.auto_advance_slide_overrides_secs.get(&index).copied()
    }

    pub fn auto_advance_effective_interval_secs(&self, index: usize) -> u32 {
        self.auto_advance_slide_override_secs(index)
            .unwrap_or(self.auto_advance_global_interval_secs)
    }

    pub fn auto_advance_profile(&self) -> (u32, BTreeMap<usize, u32>) {
        (
            self.auto_advance_global_interval_secs,
            self.auto_advance_slide_overrides_secs.clone(),
        )
    }

    pub fn set_auto_advance_profile(
        &mut self,
        global_interval_secs: u32,
        per_slide_overrides_secs: BTreeMap<usize, u32>,
        now: Instant,
    ) {
        self.auto_advance_global_interval_secs = global_interval_secs.clamp(1, 3600);
        self.auto_advance_slide_overrides_secs = per_slide_overrides_secs
            .into_iter()
            .map(|(index, secs)| (index, secs.clamp(1, 3600)))
            .collect();
        self.last_auto_tick = now;
    }

    pub fn auto_advance_remaining(&self, now: Instant) -> Option<Duration> {
        if !self.auto_advance_enabled {
            return None;
        }

        let interval = Duration::from_secs(u64::from(
            self.auto_advance_effective_interval_secs(self.current_index),
        ));
        Some(interval.saturating_sub(now.saturating_duration_since(self.last_auto_tick)))
    }

    pub fn talk_elapsed(&self, now: Instant) -> Duration {
        match self.started_at {
            Some(started_at) => now.saturating_duration_since(started_at),
            None => Duration::ZERO,
        }
    }

    pub fn current_slide_elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.current_slide_started_at)
    }

    pub fn timing_breakdown(&self, now: Instant) -> Vec<Duration> {
        let mut breakdown = self.per_slide_elapsed.clone();
        if let Some(current) = breakdown.get_mut(self.current_index) {
            *current += self.current_slide_elapsed(now);
        }
        breakdown
    }

    pub fn update_document(&mut self, total_slides: usize, labels: Vec<String>, now: Instant) {
        *self = Self::with_labels(total_slides, labels, now);
    }

    pub fn apply(&mut self, command: AppCommand, now: Instant) -> bool {
        match command {
            AppCommand::NextSlide => self.go_to(self.current_index.saturating_add(1), now),
            AppCommand::PreviousSlide => {
                let next = self.current_index.saturating_sub(1);
                self.go_to(next, now)
            }
            AppCommand::FirstSlide => self.go_to(0, now),
            AppCommand::LastSlide => self.go_to(self.total_slides.saturating_sub(1), now),
            AppCommand::GoToIndex(index) => self.go_to(index, now),
            AppCommand::GoToLabel(label) => {
                let destination = self.labels.iter().position(|value| value == &label);
                match destination {
                    Some(index) => self.go_to(index, now),
                    None => false,
                }
            }
            AppCommand::ToggleLabelMode => {
                self.use_slide_labels = !self.use_slide_labels;
                true
            }
            AppCommand::SwapScreens => {
                self.screen_roles_swapped = !self.screen_roles_swapped;
                true
            }
            AppCommand::ToggleProjectorFullscreen => {
                self.projector_fullscreen = !self.projector_fullscreen;
                true
            }
            AppCommand::ToggleLaser => {
                self.laser_enabled = !self.laser_enabled;
                true
            }
            AppCommand::ToggleHighlight => {
                self.highlight_enabled = !self.highlight_enabled;
                true
            }
            AppCommand::ToggleAutoAdvance => {
                self.auto_advance_enabled = !self.auto_advance_enabled;
                self.last_auto_tick = now;
                true
            }
            AppCommand::SetAutoAdvanceEnabled(enabled) => {
                if self.auto_advance_enabled == enabled {
                    return false;
                }
                self.auto_advance_enabled = enabled;
                self.last_auto_tick = now;
                true
            }
            AppCommand::SetAutoAdvanceGlobalInterval(secs) => {
                let clamped = secs.clamp(1, 3600);
                if self.auto_advance_global_interval_secs == clamped {
                    return false;
                }
                self.auto_advance_global_interval_secs = clamped;
                self.last_auto_tick = now;
                true
            }
            AppCommand::SetAutoAdvanceSlideOverride { index, secs } => {
                let clamped = secs.clamp(1, 3600);
                if self.auto_advance_slide_overrides_secs.get(&index).copied() == Some(clamped) {
                    return false;
                }
                self.auto_advance_slide_overrides_secs
                    .insert(index, clamped);
                self.last_auto_tick = now;
                true
            }
            AppCommand::ClearAutoAdvanceSlideOverride(index) => {
                let removed = self
                    .auto_advance_slide_overrides_secs
                    .remove(&index)
                    .is_some();
                if removed {
                    self.last_auto_tick = now;
                }
                removed
            }
            AppCommand::StartTimer => {
                if self.started_at.is_none() {
                    self.started_at = Some(now);
                    self.current_slide_started_at = now;
                    return true;
                }
                false
            }
            AppCommand::ResetTimer => {
                self.started_at = Some(now);
                self.current_slide_started_at = now;
                self.per_slide_elapsed.fill(Duration::ZERO);
                true
            }
        }
    }

    pub fn maybe_auto_advance(&mut self, now: Instant) -> bool {
        if !self.auto_advance_enabled {
            return false;
        }

        let interval = Duration::from_secs(u64::from(
            self.auto_advance_effective_interval_secs(self.current_index),
        ));
        if now.saturating_duration_since(self.last_auto_tick) < interval {
            return false;
        }
        self.last_auto_tick = now;

        let next = if self.current_index + 1 >= self.total_slides {
            0
        } else {
            self.current_index + 1
        };
        self.go_to(next, now)
    }

    fn go_to(&mut self, requested_index: usize, now: Instant) -> bool {
        let clamped = requested_index.min(self.total_slides.saturating_sub(1));
        if clamped == self.current_index {
            return false;
        }

        self.started_at.get_or_insert(now);
        let elapsed_on_slide = now.saturating_duration_since(self.current_slide_started_at);
        if let Some(current) = self.per_slide_elapsed.get_mut(self.current_index) {
            *current += elapsed_on_slide;
        }

        self.current_index = clamped;
        self.current_slide_started_at = now;
        self.last_auto_tick = now;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::PresentationState;
    use crate::app::AppCommand;
    use std::time::{Duration, Instant};

    #[test]
    fn next_previous_navigation_respects_bounds() {
        let now = Instant::now();
        let mut state =
            PresentationState::with_labels(3, vec!["1".into(), "2".into(), "3".into()], now);

        assert!(state.apply(AppCommand::NextSlide, now + Duration::from_secs(1)));
        assert_eq!(state.current_index(), 1);
        assert!(state.apply(AppCommand::NextSlide, now + Duration::from_secs(2)));
        assert_eq!(state.current_index(), 2);
        assert!(!state.apply(AppCommand::NextSlide, now + Duration::from_secs(3)));
        assert_eq!(state.current_index(), 2);
        assert!(state.apply(AppCommand::PreviousSlide, now + Duration::from_secs(4)));
        assert_eq!(state.current_index(), 1);
    }

    #[test]
    fn resolves_go_to_label() {
        let now = Instant::now();
        let mut state = PresentationState::with_labels(
            3,
            vec!["intro".into(), "agenda".into(), "qa".into()],
            now,
        );

        assert!(state.apply(
            AppCommand::GoToLabel("agenda".to_owned()),
            now + Duration::from_secs(1)
        ));
        assert_eq!(state.current_index(), 1);
        assert!(!state.apply(
            AppCommand::GoToLabel("missing".to_owned()),
            now + Duration::from_secs(2)
        ));
    }

    #[test]
    fn accumulates_per_slide_time() {
        let now = Instant::now();
        let mut state = PresentationState::with_labels(2, vec!["1".into(), "2".into()], now);

        state.apply(AppCommand::NextSlide, now + Duration::from_secs(4));
        let breakdown = state.timing_breakdown(now + Duration::from_secs(10));

        assert_eq!(breakdown[0], Duration::from_secs(4));
        assert_eq!(breakdown[1], Duration::from_secs(6));
    }

    #[test]
    fn toggles_laser_state() {
        let now = Instant::now();
        let mut state = PresentationState::with_labels(2, vec!["1".into(), "2".into()], now);
        assert!(!state.laser_enabled);
        assert!(state.apply(AppCommand::ToggleLaser, now + Duration::from_secs(1)));
        assert!(state.laser_enabled);
        assert!(state.apply(AppCommand::ToggleLaser, now + Duration::from_secs(2)));
        assert!(!state.laser_enabled);
    }

    #[test]
    fn auto_advance_uses_overrides_and_loops_to_start() {
        let now = Instant::now();
        let mut state =
            PresentationState::with_labels(3, vec!["1".into(), "2".into(), "3".into()], now);

        assert!(state.apply(AppCommand::SetAutoAdvanceGlobalInterval(10), now));
        assert!(state.apply(
            AppCommand::SetAutoAdvanceSlideOverride { index: 0, secs: 2 },
            now
        ));
        assert!(state.apply(AppCommand::SetAutoAdvanceEnabled(true), now));

        assert!(!state.maybe_auto_advance(now + Duration::from_secs(1)));
        assert!(state.maybe_auto_advance(now + Duration::from_secs(2)));
        assert_eq!(state.current_index(), 1);

        assert!(state.apply(AppCommand::GoToIndex(2), now + Duration::from_secs(3)));
        assert!(state.apply(
            AppCommand::SetAutoAdvanceSlideOverride { index: 2, secs: 1 },
            now + Duration::from_secs(3),
        ));
        assert!(state.maybe_auto_advance(now + Duration::from_secs(4)));
        assert_eq!(state.current_index(), 0);
    }

    #[test]
    fn auto_advance_remaining_reports_countdown() {
        let now = Instant::now();
        let mut state = PresentationState::with_labels(2, vec!["1".into(), "2".into()], now);
        state.apply(AppCommand::SetAutoAdvanceGlobalInterval(5), now);
        state.apply(AppCommand::SetAutoAdvanceEnabled(true), now);

        let remaining = state
            .auto_advance_remaining(now + Duration::from_secs(2))
            .expect("countdown should be available when auto-advance is enabled");
        assert_eq!(remaining, Duration::from_secs(3));
    }
}
