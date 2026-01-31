use crate::review_panel_settings::ReviewPanelSettings;
use anyhow::Result;
use fs::Fs;
use gpui::{
    App, AsyncWindowContext, Context, Corner, Entity, EventEmitter, FocusHandle, Focusable, Pixels,
    Render, WeakEntity, Window,
};
use settings::{self, Settings};
use std::sync::Arc;
use ui::{
    Color, ContextMenu, DynamicSpacing, IconButton, IconName, IconSize, IntoElement, Label,
    LabelSize, PopoverMenu, PopoverMenuHandle, Tab, Tooltip, h_flex, prelude::*, v_flex,
};
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};
use zed_actions::review_panel::ToggleFocus;

const REVIEW_PANEL_KEY: &str = "ReviewPanel";

enum ActiveView {
    Empty,
    PullRequestList,
    ReviewThread,
    FileList,
    Configuration,
}

pub struct ReviewPanel {
    _workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    recent_reviews_menu_handle: PopoverMenuHandle<ContextMenu>,
    options_menu_handle: PopoverMenuHandle<ContextMenu>,
    fs: Arc<dyn Fs>,
    width: Option<Pixels>,
    active_view: ActiveView,
}

pub fn register(workspace: &mut Workspace) {
    workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
        workspace.toggle_panel_focus::<ReviewPanel>(window, cx);
    });
}

impl ReviewPanel {
    pub fn new(
        workspace: &Workspace,
        weak_workspace: WeakEntity<Workspace>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let fs = workspace.app_state().fs.clone();
        Self {
            _workspace: weak_workspace,
            focus_handle: cx.focus_handle(),
            fs,
            width: None,
            recent_reviews_menu_handle: PopoverMenuHandle::default(),
            options_menu_handle: PopoverMenuHandle::default(),
            active_view: ActiveView::Empty,
        }
    }

    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let weak_workspace = workspace.weak_handle();
            cx.new(|cx| ReviewPanel::new(workspace, weak_workspace, window, cx))
        })
    }

    fn set_active_view(&mut self, new_view: ActiveView, cx: &mut Context<Self>) {
        self.active_view = new_view;
        cx.notify();
    }

    fn render_recent_reviews_menu(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        PopoverMenu::new("review-nav-menu")
            .trigger_with_tooltip(
                IconButton::new("review-nav-menu", IconName::MenuAltTemp)
                    .icon_size(IconSize::Small),
                Tooltip::text("Recent Reviews"),
            )
            .anchor(Corner::TopRight)
            .with_handle(self.recent_reviews_menu_handle.clone())
            .menu(move |window, cx| {
                Some(ContextMenu::build(window, cx, |menu, _window, _| {
                    menu.entry("No recent reviews", None, |_window, _cx| {})
                }))
            })
    }

    fn render_options_menu(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        PopoverMenu::new("review-options-menu")
            .trigger_with_tooltip(
                IconButton::new("review-options-menu", IconName::EllipsisVertical)
                    .icon_size(IconSize::Small),
                Tooltip::text("Options"),
            )
            .anchor(Corner::TopRight)
            .with_handle(self.options_menu_handle.clone())
            .menu(move |window, cx| {
                Some(ContextMenu::build(window, cx, |menu, _window, _| {
                    menu.entry("Configuration", None, |_window, _cx| {
                        // TODO: dispatch OpenConfiguration action
                    })
                    .separator()
                    .entry("Full Screen", None, |_window, _cx| {
                        // TODO: dispatch ToggleZoom action
                    })
                }))
            })
    }

    fn render_toolbar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .id("review-panel-toolbar")
            .h(Tab::container_height(cx))
            .max_w_full()
            .flex_none()
            .justify_between()
            .gap_2()
            .bg(cx.theme().colors().tab_bar_background)
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(
                h_flex()
                    .size_full()
                    .gap(DynamicSpacing::Base04.rems(cx))
                    .pl(DynamicSpacing::Base04.rems(cx))
                    .child(Label::new("Review").size(LabelSize::Small)),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap(DynamicSpacing::Base02.rems(cx))
                    .pr(DynamicSpacing::Base06.rems(cx))
                    .child(
                        IconButton::new("new-review", IconName::Plus)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("New Review"))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.set_active_view(ActiveView::PullRequestList, cx);
                            })),
                    )
                    .child(self.render_recent_reviews_menu(cx))
                    .child(self.render_options_menu(window, cx)),
            )
    }
}

impl Render for ReviewPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("review_panel")
            .track_focus(&self.focus_handle)
            .size_full()
            .child(self.render_toolbar(window, cx))
            .map(|parent| match &self.active_view {
                ActiveView::Empty => parent.child(
                    v_flex()
                        .size_full()
                        .justify_center()
                        .items_center()
                        .child(Label::new("No review selected").color(Color::Muted)),
                ),
                ActiveView::PullRequestList => parent.child(
                    v_flex()
                        .size_full()
                        .justify_center()
                        .items_center()
                        .child(Label::new("PR List (coming soon)").color(Color::Muted)),
                ),
                ActiveView::ReviewThread => parent.child(
                    v_flex()
                        .size_full()
                        .justify_center()
                        .items_center()
                        .child(Label::new("Review Thread (coming soon)").color(Color::Muted)),
                ),
                ActiveView::FileList => parent.child(
                    v_flex()
                        .size_full()
                        .justify_center()
                        .items_center()
                        .child(Label::new("Changed Files (coming soon)").color(Color::Muted)),
                ),
                ActiveView::Configuration => parent.child(
                    v_flex()
                        .size_full()
                        .justify_center()
                        .items_center()
                        .child(Label::new("Configuration (coming soon)").color(Color::Muted)),
                ),
            })
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
