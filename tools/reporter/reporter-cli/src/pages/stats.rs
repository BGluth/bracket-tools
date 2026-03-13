use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Block, Borders, Paragraph, Widget, WidgetRef},
};
use reporter_state::stores::{store_utils::StoreDataRef, tourney::TourneyStore};

#[derive(Debug)]
pub(crate) struct Root {
    tourney_store: StoreDataRef<TourneyStore>,
}

impl Root {
    pub(crate) fn new(tourney_store: StoreDataRef<TourneyStore>) -> Self {
        todo!()
    }
}

impl WidgetRef for Root {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage((1.0 / 0.67) as u16), Constraint::Fill(1)]);

        let header_text = format!("{} ({})", self.tourney_store.tourney_name(), self.tourney_store.get_total_players());

        let header = Paragraph::new(header_text).block(Block::new().borders(Borders::all()));

        header.render(area, buf);
    }
}

#[derive(Debug)]
struct StatsWidget {}

impl WidgetRef for StatsWidget {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        todo!()
    }
}
