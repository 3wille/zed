use crate::review_panel_settings::ReviewPanelSettings;
use anyhow::Result;
use collections::HashSet;
use fs::Fs;
use git::repository::RepoPath;
use git::status::{DiffTreeType, TreeDiff, TreeDiffStatus};
use gpui::{
    App, AsyncWindowContext, Context, Corner, Entity, EventEmitter, FocusHandle, Focusable, Pixels,
    Render, WeakEntity, Window,
};
use project::{
    Project,
    git_store::{GitStoreEvent, Repository, RepositoryEvent},
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

#[derive(Clone)]
struct RecentReview {
    base_branch: SharedString,
    head_branch: SharedString,
    file_count: usize,
}

enum ActiveView {
    Empty,
    PullRequestList,
    ReviewThread,
    FileList,
    Configuration,
}

pub struct ReviewPanel {
    _workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    active_repository: Option<Entity<Repository>>,
    base_branch: Option<SharedString>,
    head_branch: Option<SharedString>,
    tree_diff: Option<TreeDiff>,
    viewed_files: HashSet<RepoPath>,
    selected_entry: Option<usize>,
    focus_handle: FocusHandle,
    recent_reviews_menu_handle: PopoverMenuHandle<ContextMenu>,
    options_menu_handle: PopoverMenuHandle<ContextMenu>,
    fs: Arc<dyn Fs>,
    width: Option<Pixels>,
    active_view: ActiveView,
    recent_reviews: Vec<RecentReview>,
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let fs = workspace.app_state().fs.clone();
        let project = workspace.project().clone();
        let active_repository = project.read(cx).active_repository(cx);
        let git_store = project.read(cx).git_store().clone();
        cx.subscribe_in(
            &git_store,
            window,
            |this, _store, event, _window, cx| match event {
                GitStoreEvent::ActiveRepositoryChanged(_) => {
                    this.active_repository = this.project.read(cx).active_repository(cx);
                    this.load_branches(cx);
                    cx.notify();
                }
                GitStoreEvent::RepositoryUpdated(_, RepositoryEvent::BranchChanged, _) => {
                    this.load_branches(cx);
                }
                _ => {}
            },
        )
        .detach();

        let mut this = Self {
            _workspace: weak_workspace,
            project,
            active_repository,
            base_branch: None,
            head_branch: None,
            tree_diff: None,
            viewed_files: HashSet::default(),
            selected_entry: None,
            focus_handle: cx.focus_handle(),
            fs,
            width: None,
            recent_reviews_menu_handle: PopoverMenuHandle::default(),
            options_menu_handle: PopoverMenuHandle::default(),
            active_view: ActiveView::Empty,
            recent_reviews: Vec::new(),
        };
        this.load_branches(cx);
        this
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

    fn render_recent_reviews_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let recent = self.recent_reviews.clone();
        let weak_panel = cx.weak_entity();

        PopoverMenu::new("review-nav-menu")
            .trigger_with_tooltip(
                IconButton::new("review-nav-menu", IconName::MenuAltTemp)
                    .icon_size(IconSize::Small),
                Tooltip::text("Recent Reviews"),
            )
            .anchor(Corner::TopRight)
            .with_handle(self.recent_reviews_menu_handle.clone())
            .menu(move |window, cx| {
                let recent = recent.clone();
                let weak_panel = weak_panel.clone();
                Some(ContextMenu::build(
                    window,
                    cx,
                    move |mut menu, _window, _cx| {
                        if recent.is_empty() {
                            return menu.entry("No recent reviews", None, |_window, _cx| {});
                        }

                        menu = menu.header("Recent");
                        for entry in &recent {
                            let label = format!(
                                "{}..{} ({} files)",
                                entry.base_branch, entry.head_branch, entry.file_count
                            );
                            let base = entry.base_branch.clone();
                            let head = entry.head_branch.clone();
                            let weak_panel = weak_panel.clone();
                            menu = menu.entry(label, None, move |_window, cx| {
                                weak_panel
                                    .update(cx, |this, cx| {
                                        this.base_branch = Some(base.clone());
                                        this.head_branch = Some(head.clone());
                                        this.load_diff(cx);
                                    })
                                    .ok();
                            });
                        }
                        menu
                    },
                ))
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

    fn load_branches(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.active_repository.clone() else {
            return;
        };

        let default_branch_rx = repo.update(cx, |repo, _cx| repo.default_branch(false));
        let branches_rx = repo.update(cx, |repo, _cx| repo.branches());

        cx.spawn(async move |this, cx| {
            if let Ok(Some(default)) = default_branch_rx.await? {
                this.update(cx, |this, cx| {
                    this.base_branch = Some(default);
                    cx.notify();
                })?;
            }

            if let Ok(branches) = branches_rx.await? {
                let head = branches
                    .iter()
                    .find(|b| b.is_head)
                    .map(|b| b.ref_name.clone());
                if let Some(head) = head {
                    this.update(cx, |this, cx| {
                        this.head_branch = Some(head);
                        this.load_diff(cx);
                        cx.notify();
                    })?;
                }
            }

            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn load_diff(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.active_repository.clone() else {
            return;
        };

        let Some(base) = self.base_branch.clone() else {
            return;
        };

        let Some(head) = self.head_branch.clone() else {
            return;
        };

        let diff_rx = repo.update(cx, |repo, cx| {
            repo.diff_tree(DiffTreeType::MergeBase { base, head }, cx)
        });

        cx.spawn(async move |this, cx| {
            let tree_diff = diff_rx.await??;
            this.update(cx, |this, cx| {
                this.tree_diff = Some(tree_diff);

                //save to recent reviews
                let file_count = this
                    .tree_diff
                    .as_ref()
                    .map(|d| d.entries.len())
                    .unwrap_or(0);

                if let (Some(base), Some(head)) =
                    (this.base_branch.clone(), this.head_branch.clone())
                {
                    this.recent_reviews
                        .retain(|r| !(r.base_branch == base && r.head_branch == head));
                    this.recent_reviews.insert(
                        0,
                        RecentReview {
                            base_branch: base,
                            head_branch: head,
                            file_count,
                        },
                    );
                }

                this.viewed_files.clear();
                this.selected_entry = None;
                this.set_active_view(ActiveView::FileList, cx);
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn open_file_diff(&mut self, path: RepoPath, window: &mut Window, cx: &mut Context<Self>) {
        self.viewed_files.insert(path.clone());
        let entries = self.sorted_entries();
        self.selected_entry = entries.iter().position(|(p, _)| *p == path);
        cx.notify();

        let Some(workspace) = self._workspace.upgrade() else {
            return;
        };
        let Some(active_repo) = self.active_repository.as_ref() else {
            return;
        };
        let Some(project_path) = active_repo.read(cx).repo_path_to_project_path(&path, cx) else {
            return;
        };

        let existing = workspace
            .read(cx)
            .items_of_type::<git_ui::project_diff::ProjectDiff>(cx)
            .find(|item| {
                matches!(
                    item.read(cx).diff_base(cx),
                    project::git_store::branch_diff::DiffBase::Merge { .. }
                )
            });

        if let Some(existing) = existing {
            workspace.update(cx, |workspace, cx| {
                workspace.activate_item(&existing, true, true, window, cx);
            });
            existing.update(cx, |diff, cx| {
                diff.move_to_project_path(&project_path, window, cx);
            });
        } else {
            window.dispatch_action(Box::new(git_ui::project_diff::BranchDiff), cx);
            let weak_self = cx.weak_entity();
            cx.spawn_in(window, async move |_, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(500))
                    .await;
                weak_self
                    .update_in(cx, |this, window, cx| {
                        this.open_file_diff(path, window, cx);
                    })
                    .ok();
                anyhow::Ok(())
            })
            .detach_and_log_err(cx);
        }
    }

    fn entry_count(&self) -> usize {
        self.tree_diff
            .as_ref()
            .map(|d| d.entries.len())
            .unwrap_or(0)
    }

    fn sorted_entries(&self) -> Vec<(RepoPath, TreeDiffStatus)> {
        let Some(tree_diff) = &self.tree_diff else {
            return Vec::new();
        };
        let mut entries: Vec<_> = tree_diff
            .entries
            .iter()
            .map(|(p, s)| (p.clone(), s.clone()))
            .collect();
        entries.sort_by(|(path_a, status_a), (path_b, status_b)| {
            let order = |s: &TreeDiffStatus| match s {
                TreeDiffStatus::Added => 0,
                TreeDiffStatus::Modified { .. } => 1,
                TreeDiffStatus::Deleted { .. } => 2,
            };
            order(status_a)
                .cmp(&order(status_b))
                .then(path_a.cmp(path_b))
        });
        entries
    }

    fn select_next(&mut self, _: &menu::SelectNext, _window: &mut Window, cx: &mut Context<Self>) {
        let count = self.entry_count();
        if count == 0 {
            return;
        }
        let next = match self.selected_entry {
            Some(current) if current + 1 < count => current + 1,
            None => 0,
            _ => return,
        };
        self.selected_entry = Some(next);
        cx.notify();
    }

    fn select_previous(
        &mut self,
        _: &menu::SelectPrevious,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.entry_count();
        if count == 0 {
            return;
        }
        let prev = match self.selected_entry {
            Some(current) if current > 0 => current - 1,
            None => 0,
            _ => return,
        };
        self.selected_entry = Some(prev);
        cx.notify();
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected) = self.selected_entry else {
            return;
        };
        let entries = self.sorted_entries();
        if let Some((path, _)) = entries.get(selected) {
            let path = path.clone();
            self.open_file_diff(path, window, cx);
        }
    }

    fn render_file_list(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.tree_diff.is_none() {
            return v_flex()
                .size_full()
                .justify_center()
                .items_center()
                .child(Label::new("Loading...").color(Color::Muted))
                .into_any_element();
        }

        let entries = self.sorted_entries();

        let header_text = format!(
            "{} <- {}",
            self.base_branch.as_ref().map(|s| s.as_ref()).unwrap_or("?"),
            self.head_branch.as_ref().map(|s| s.as_ref()).unwrap_or("?"),
        );

        let file_count = entries.len();
        let added = entries
            .iter()
            .filter(|(_, s)| matches!(s, TreeDiffStatus::Added))
            .count();
        let modified = entries
            .iter()
            .filter(|(_, s)| matches!(s, TreeDiffStatus::Modified { .. }))
            .count();
        let deleted = entries
            .iter()
            .filter(|(_, s)| matches!(s, TreeDiffStatus::Deleted { .. }))
            .count();
        let viewed_count = self.viewed_files.len();

        let summary = format!(
            "{} changed files (+{} -{} ~{}) — {}/{} viewed",
            file_count, added, deleted, modified, viewed_count, file_count
        );

        let info_color = cx.theme().status().info;
        let selected_bg_alpha = 0.08;

        v_flex()
            .id("review-file-list")
            .size_full()
            .overflow_scroll()
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::confirm))
            .child(
                h_flex().px_2().py_1().child(
                    Label::new(header_text)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
            )
            .child(
                h_flex().px_2().pb_1().child(
                    Label::new(summary)
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                ),
            )
            .children(entries.into_iter().enumerate().map(|(ix, (path, status))| {
                let (icon, color) = match &status {
                    TreeDiffStatus::Added => (IconName::Plus, Color::Created),
                    TreeDiffStatus::Modified { .. } => (IconName::Pencil, Color::Modified),
                    TreeDiffStatus::Deleted { .. } => (IconName::Dash, Color::Deleted),
                };

                let is_selected = self.selected_entry == Some(ix);
                let is_viewed = self.viewed_files.contains(&path);
                let label_color = if is_viewed {
                    Color::Muted
                } else {
                    Color::Default
                };

                let bg = if is_selected {
                    info_color.alpha(selected_bg_alpha)
                } else {
                    cx.theme().colors().ghost_element_background
                };

                let hover_bg = if is_selected {
                    info_color.alpha(selected_bg_alpha + 0.04)
                } else {
                    cx.theme().colors().ghost_element_hover
                };

                h_flex()
                    .id(SharedString::from(format!("file_entry_{}", ix)))
                    .px_2()
                    .py_1()
                    .gap_2()
                    .rounded_md()
                    .bg(bg)
                    .hover(move |style| style.bg(hover_bg))
                    .when(!is_viewed, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .w(px(6.))
                                .h(px(6.))
                                .rounded_full()
                                .bg(cx.theme().status().info),
                        )
                    })
                    .when(is_viewed, |row| {
                        row.child(div().flex_none().w(px(6.)).h(px(6.)))
                    })
                    .child(Icon::new(icon).size(IconSize::Small).color(color))
                    .child(
                        div().overflow_x_hidden().child(
                            Label::new(path.as_std_path().to_string_lossy().to_string())
                                .size(LabelSize::Small)
                                .color(label_color)
                                .single_line(),
                        ),
                    )
                    .on_click({
                        let path = path.clone();
                        cx.listener(move |this, _event, window, cx| {
                            this.open_file_diff(path.clone(), window, cx);
                        })
                    })
            }))
            .into_any_element()
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
                ActiveView::FileList => parent.child(self.render_file_list(cx)),
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
