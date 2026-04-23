#[derive(Debug, Clone)]
pub(super) struct DeckOverviewState {
    pub(super) is_open: bool,
    pub(super) selected_index: usize,
    pub(super) thumbnail_scale: f32,
    pub(super) commit_pending: Option<usize>,
}

impl Default for DeckOverviewState {
    fn default() -> Self {
        Self {
            is_open: false,
            selected_index: 0,
            thumbnail_scale: 1.0,
            commit_pending: None,
        }
    }
}

impl DeckOverviewState {
    pub(super) fn open(&mut self, current_index: usize) {
        self.is_open = true;
        self.selected_index = current_index;
    }

    pub(super) fn toggle(&mut self, current_index: usize) {
        if self.is_open {
            self.is_open = false;
        } else {
            self.open(current_index);
        }
    }

    pub(super) fn cancel(&mut self) {
        self.is_open = false;
        self.commit_pending = None;
    }

    pub(super) fn move_selection(
        &mut self,
        total_slides: usize,
        columns: usize,
        delta_col: i32,
        delta_row: i32,
    ) {
        if total_slides == 0 {
            self.selected_index = 0;
            return;
        }
        let cols = columns.max(1);
        let mut row = self.selected_index / cols;
        let mut col = self.selected_index % cols;

        if delta_col < 0 {
            col = col.saturating_sub(delta_col.unsigned_abs() as usize);
        } else if delta_col > 0 {
            col = col.saturating_add(delta_col as usize);
        }

        if delta_row < 0 {
            row = row.saturating_sub(delta_row.unsigned_abs() as usize);
        } else if delta_row > 0 {
            row = row.saturating_add(delta_row as usize);
        }

        let mut next = row.saturating_mul(cols).saturating_add(col);
        if next >= total_slides {
            next = total_slides - 1;
        }
        self.selected_index = next;
    }

    pub(super) fn commit(&mut self) {
        self.commit_pending = Some(self.selected_index);
        self.is_open = false;
    }

    pub(super) fn take_commit(&mut self) -> Option<usize> {
        self.commit_pending.take()
    }
}
