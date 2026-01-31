use crate::review_panel_settings::ReviewPanelSettings;
use anyhow::Result;
use fs::Fs;
use gpui::{
    App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable, Pixels, Render,
    WeakEntity, Window,
};
use settings::{self, Settings};
use std::sync::Arc;
use ui::prelude::*;
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};
use zed_actions::review_panel::ToggleFocus;

const REVIEW_PANEL_KEY: &str = "ReviewPanel";

pub struct ReviewPanel {
    focus_handle: FocusHandle,
    fs: Arc<dyn Fs>,
    width: Option<Pixels>,
}

pub fn register(workspace: &mut Workspace) {
    workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
        workspace.toggle_panel_focus::<ReviewPanel>(window, cx);
    });
}

impl ReviewPanel {
    pub fn new(workspace: &Workspace, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let fs = workspace.app_state().fs.clone();
        Self {
            focus_handle: cx.focus_handle(),
            fs,
            width: None,
        }
    }

    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            cx.new(|cx| ReviewPanel::new(workspace, window, cx))
        })
    }
}

impl Render for ReviewPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id(REVIEW_PANEL_KEY)
            .track_focus(&self.focus_handle)
            .size_full()
            .justify_center()
            .items_center()
            .child(Label::new("Review Panel").color(Color::Muted))
    }
}

impl Focusable for ReviewPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for ReviewPanel {}

impl Panel for ReviewPanel {
    fn persistent_name() -> &'static str {
        "ReviewPanel"
    }

    fn panel_key() -> &'static str {
        REVIEW_PANEL_KEY
    }

    fn position(&self, _window: &Window, cx: &App) -> DockPosition {
        ReviewPanelSettings::get_global(cx).dock
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(
        &mut self,
        position: DockPosition,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        settings::update_settings_file(self.fs.clone(), cx, move |settings, _| {
            settings.review_panel.get_or_insert_default().dock = Some(position.into())
        });
    }

    fn size(&self, _window: &Window, cx: &App) -> Pixels {
        self.width
            .unwrap_or_else(|| ReviewPanelSettings::get_global(cx).default_width)
    }

    fn set_size(&mut self, size: Option<Pixels>, _window: &mut Window, cx: &mut Context<Self>) {
        self.width = size;
        cx.notify();
    }

    fn icon(&self, _window: &Window, cx: &App) -> Option<ui::IconName> {
        Some(ui::IconName::PullRequest).filter(|_| ReviewPanelSettings::get_global(cx).button)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Review Panel")
    }

    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(ToggleFocus)
    }

    fn activation_priority(&self) -> u32 {
        10
    }
}
