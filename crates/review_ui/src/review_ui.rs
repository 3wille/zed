mod review_panel;

pub use review_panel::ReviewPanel;

use gpui::App;
use workspace::Workspace;

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        review_panel::register(workspace);
    })
    .detach();
}
