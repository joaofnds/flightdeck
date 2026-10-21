use persistence::Result;

use super::LiveSession;
use crate::app_process_groups_history::AppProcessGroupsHistory;
use crate::live_app_history::Target;
use crate::live_app_response::KeyKind;

impl LiveSession {
    /// Seven context-mean CPU leaders and Other, sharing one exact selected-App timeline.
    pub fn app_process_groups_history(
        &self,
        kind: KeyKind,
        app_bytes: &[u8],
    ) -> Result<Box<AppProcessGroupsHistory>> {
        self.operation.run(|| {
            let selected = self.selected_in_context()?;
            let target = Target {
                kind,
                bytes: app_bytes,
            };

            self.app_history.process_groups_response(
                &selected,
                &target,
                self.history.as_ref(),
                &self.selected_context,
                &self.operation,
            )
        })
    }
}
