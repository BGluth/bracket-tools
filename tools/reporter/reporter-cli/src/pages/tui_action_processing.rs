use reporter_state::stores::store_action_processing::StoreAction;

/// Converts a `TUI` action into a `Store` action.
#[derive(Debug, Default)]
pub(crate) struct TuiActionProcessing {}

#[derive(Clone, Debug)]
pub(crate) enum TuiAction {}

impl TuiActionProcessing {
    /// Process a TUI action into 0 - * store actions.
    pub(crate) fn process_action(action: TuiAction) -> Box<dyn Iterator<Item = StoreAction>> {
        todo!()
    }
}
