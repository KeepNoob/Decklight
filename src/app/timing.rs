use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub(super) struct TimingBreakdownState {
    pub(super) is_open: bool,
}

impl TimingBreakdownState {
    pub(super) fn toggle(&mut self) {
        self.is_open = !self.is_open;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TimingBreakdownRow {
    SectionTotal {
        depth: usize,
        title: String,
        duration: Duration,
    },
    Slide {
        depth: usize,
        index: usize,
        label: String,
        duration: Duration,
        is_current: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TimingBreakdownModel {
    pub(super) total: Duration,
    pub(super) has_sections: bool,
    pub(super) rows: Vec<TimingBreakdownRow>,
}
