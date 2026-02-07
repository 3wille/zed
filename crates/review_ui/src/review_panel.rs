use crate::file_list::{FileList, FileListEvent};
use crate::github_provider::GitHubProvider;
use crate::pull_request_list::{PullRequestList, PullRequestListEvent};
use crate::review_view::{ReviewView, ReviewViewEvent};
use crate::github_token::resolve_github_token;
use crate::review_panel_settings::ReviewPanelSettings;
use crate::review_provider::{PullRequestInfo, ReviewProvider};
use anyhow::Result;
use credentials_provider::CredentialsProvider;
use fs::Fs;
use git::repository::RepoPath;
use git::status::{DiffTreeType, TreeDiff};
use gpui::{
    App, AsyncWindowContext, Context, Corner, Entity, EventEmitter, FocusHandle, Focusable, Pixels,
    Render, SharedString, Subscription, WeakEntity, Window,
};
use http_client::HttpClient;
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
    pull_request: Option<PullRequestInfo>,
}

enum ActiveView {
    Empty,
    PullRequestList,
    ReviewThread,
    FileList,
    Configuration,
}

enum PendingAction {
    OpenDiff(RepoPath),
    OpenLocal(RepoPath),
    SelectPullRequest(PullRequestInfo),
}

pub struct ReviewPanel {
    _workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    active_repository: Option<Entity<Repository>>,
    base_branch: Option<SharedString>,
    head_branch: Option<SharedString>,
    tree_diff: Option<TreeDiff>,
    file_list: Option<(Entity<FileList>, Subscription)>,
    review_view: Option<(Entity<ReviewView>, Subscription)>,
    focus_handle: FocusHandle,
    recent_reviews_menu_handle: PopoverMenuHandle<ContextMenu>,
    options_menu_handle: PopoverMenuHandle<ContextMenu>,
    fs: Arc<dyn Fs>,
    width: Option<Pixels>,
    active_view: ActiveView,
    recent_reviews: Vec<RecentReview>,
    http_client: Arc<dyn HttpClient>,
    pull_request_list: Option<(Entity<PullRequestList>, Subscription)>,
    provider: Option<Arc<dyn ReviewProvider>>,
    remote_owner: Option<String>,
    remote_repo: Option<String>,
    selected_pr: Option<PullRequestInfo>,
    pr_ref_fetch_task: Option<gpui::Task<Result<()>>>,
    pending_action: Option<PendingAction>,
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
                    this.initialize_provider(cx);
                    this.load_branches(cx);
                    cx.notify();
                }
                GitStoreEvent::RepositoryUpdated(_, RepositoryEvent::BranchChanged, _) => {
                    this.load_branches(cx);
                }
                GitStoreEvent::RepositoryUpdated(..) => {
                    if this.provider.is_none() {
                        this.initialize_provider(cx);
                    }
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
            file_list: None,
            review_view: None,
            focus_handle: cx.focus_handle(),
            fs,
            width: None,
            recent_reviews_menu_handle: PopoverMenuHandle::default(),
            options_menu_handle: PopoverMenuHandle::default(),
            active_view: ActiveView::Empty,
            recent_reviews: Vec::new(),
            http_client: workspace.client().http_client().clone(),
            pull_request_list: None,
            provider: None,
            remote_owner: None,
            remote_repo: None,
            selected_pr: None,
            pr_ref_fetch_task: None,
            pending_action: None,
        };
        this.initialize_provider(cx);
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

    fn refresh_pull_requests(&mut self, cx: &mut Context<Self>) {
        if let Some((pr_list, _)) = &self.pull_request_list {
            pr_list.update(cx, |list, cx| list.refresh(cx));
        }
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
                            let label = if let Some(pr) = &entry.pull_request {
                                format!(
                                    "#{} {}..{} ({} files)",
                                    pr.number,
                                    entry.base_branch,
                                    entry.head_branch,
                                    entry.file_count
                                )
                            } else {
                                format!(
                                    "{}..{} ({} files)",
                                    entry.base_branch,
                                    entry.head_branch,
                                    entry.file_count
                                )
                            };
                            let base = entry.base_branch.clone();
                            let head = entry.head_branch.clone();
                            let pull_request = entry.pull_request.clone();
                            let weak_panel = weak_panel.clone();
                            menu = menu.entry(label, None, move |_window, cx| {
                                weak_panel
                                    .update(cx, |this, cx| {
                                        this.base_branch = Some(base.clone());
                                        this.head_branch = Some(head.clone());
                                        if let Some(pr) = &pull_request {
                                            this.select_pull_request(pr, cx);
                                        } else {
                                            this.selected_pr = None;
                                            this.review_view = None;
                                            this.load_diff(cx);
                                        }
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
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let weak_panel = cx.weak_entity();
        let show_tree_toggle = matches!(
            self.active_view,
            ActiveView::FileList | ActiveView::ReviewThread
        );
        let is_tree_view = match &self.active_view {
            ActiveView::FileList => self
                .file_list
                .as_ref()
                .map(|(fl, _)| fl.read(cx).is_tree_view())
                .unwrap_or(false),
            ActiveView::ReviewThread => self
                .review_view
                .as_ref()
                .map(|(rv, _)| rv.read(cx).is_tree_view())
                .unwrap_or(false),
            _ => false,
        };
        let is_review_thread = matches!(self.active_view, ActiveView::ReviewThread);

        PopoverMenu::new("review-options-menu")
            .trigger_with_tooltip(
                IconButton::new("review-options-menu", IconName::EllipsisVertical)
                    .icon_size(IconSize::Small),
                Tooltip::text("Options"),
            )
            .anchor(Corner::TopRight)
            .with_handle(self.options_menu_handle.clone())
            .menu(move |window, cx| {
                let weak_panel = weak_panel.clone();
                Some(ContextMenu::build(window, cx, move |menu, _window, _| {
                    menu.when(show_tree_toggle, |menu| {
                        let weak_panel = weak_panel.clone();
                        menu.entry(
                            if is_tree_view {
                                "Flat View"
                            } else {
                                "Tree View"
                            },
                            None,
                            {
                                let weak_panel = weak_panel.clone();
                                move |window, cx| {
                                    weak_panel
                                        .update(cx, |this, cx| {
                                            if is_review_thread {
                                                if let Some((review_view, _)) = &this.review_view {
                                                    review_view.update(cx, |rv, cx| {
                                                        rv.toggle_tree_view(cx);
                                                    });
                                                }
                                            } else if let Some((file_list, _)) = &this.file_list {
                                                file_list.update(cx, |fl, cx| {
                                                    fl.toggle_tree_view(window, cx);
                                                });
                                            }
                                        })
                                        .ok();
                                }
                            },
                        )
                        .separator()
                    })
                    .entry("Configuration", None, |_window, _cx| {
                        // TODO: dispatch OpenConfiguration action
                    })
                    .separator()
                    .entry("Full Screen", None, |_window, _cx| {
                        // TODO: dispatch ToggleZoom action
                    })
                    .separator()
                    .entry("Refresh", None, {
                        let weak_panel = weak_panel.clone();
                        move |_window, cx| {
                            weak_panel
                                .update(cx, |this, cx| {
                                    this.refresh_pull_requests(cx);
                                })
                                .ok();
                        }
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
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.show_pull_request_list(window, cx);
                            })),
                    )
                    .child(self.render_recent_reviews_menu(cx))
                    .child(self.render_options_menu(window, cx)),
            )
    }

    fn initialize_provider(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.active_repository.clone() else {
            log::info!("review_panel: no active repository");
            return;
        };

        let remote_url = repo.read(cx).default_remote_url();
        log::info!("review_panel: remote_url = {:?}", remote_url);
        let Some(remote_url) = remote_url else {
            log::info!("review_panel: no remote URL found");
            return;
        };

        let Ok((owner, repo_name)) = parse_github_remote(&remote_url) else {
            log::info!("review_panel: failed to parse remote URL: {}", remote_url);
            return;
        };
        log::info!("review_panel: parsed {}/{}", owner, repo_name);

        let http_client = self.http_client.clone();
        let credentials_provider = <dyn CredentialsProvider>::global(cx);

        self.remote_owner = Some(owner);
        self.remote_repo = Some(repo_name);

        cx.spawn(async move |this, cx| {
            let token = resolve_github_token(credentials_provider, cx).await;

            let provider: Arc<dyn ReviewProvider> =
                Arc::new(GitHubProvider::new(http_client, token));

            this.update(cx, |this, cx| {
                this.provider = Some(provider.clone());
                if let (Some((pr_list, _)), Some(owner), Some(repo)) =
                    (&this.pull_request_list, this.remote_owner.clone(), this.remote_repo.clone())
                {
                    pr_list.update(cx, |list, cx| {
                        list.set_provider(provider, owner, repo, cx);
                    });
                }
                cx.notify();
            })?;

            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn select_pull_request(&mut self, pr: &PullRequestInfo, cx: &mut Context<Self>) {
        self.selected_pr = Some(pr.clone());
        self.base_branch = Some(pr.base_ref.clone());
        self.head_branch = Some(pr.head_ref.clone());

        self.fetch_pr_ref(pr.number, cx);
        self.pending_action = Some(PendingAction::SelectPullRequest(pr.clone()));
        self.set_active_view(ActiveView::ReviewThread, cx);
    }

    fn create_review_view(
        &mut self,
        pr: &PullRequestInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let review_view = cx.new(|cx| {
            ReviewView::new(
                self.provider.clone(),
                self.remote_owner.clone(),
                self.remote_repo.clone(),
                pr.clone(),
                window,
                cx,
            )
        });
        let subscription =
            cx.subscribe_in(&review_view, window, |this, _view, event, window, cx| {
                match event {
                    ReviewViewEvent::OpenFileDiff(path) => {
                        this.open_file_diff(path.clone(), cx);
                    }
                    ReviewViewEvent::Back => {
                        this.selected_pr = None;
                        this.review_view = None;
                        this.show_pull_request_list(window, cx);
                    }
                }
            });
        self.review_view = Some((review_view, subscription));
    }

    fn fetch_pr_ref(&mut self, pr_number: u32, cx: &mut Context<Self>) {
        let Some(repo) = self.active_repository.clone() else {
            return;
        };

        let work_dir = repo.read(cx).snapshot().work_directory_abs_path.clone();
        let refspec = format!("pull/{}/head", pr_number);

        self.pr_ref_fetch_task = Some(cx.spawn(async move |this, cx| {
            let output = smol::process::Command::new("git")
                .current_dir(work_dir.as_ref())
                .args(["fetch", "origin", &refspec])
                .output()
                .await?;

            if output.status.success() {
                log::info!("review_panel: fetched PR #{} ref", pr_number);
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                log::warn!(
                    "review_panel: PR #{} ref fetch failed: {}",
                    pr_number,
                    stderr
                );
            }

            this.update(cx, |this, cx| {
                this.load_diff(cx);
            })?;
            anyhow::Ok(())
        }));
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

                let file_count = this
                    .tree_diff
                    .as_ref()
                    .map(|d| d.entries.len())
                    .unwrap_or(0);

                if let (Some(base), Some(head)) =
                    (this.base_branch.clone(), this.head_branch.clone())
                {
                    let pull_request = this.selected_pr.clone();
                    this.recent_reviews
                        .retain(|r| !(r.base_branch == base && r.head_branch == head));
                    this.recent_reviews.insert(
                        0,
                        RecentReview {
                            base_branch: base,
                            head_branch: head,
                            file_count,
                            pull_request,
                        },
                    );
                    this.recent_reviews.truncate(10);
                }

                if let Some((review_view, _)) = &this.review_view {
                    review_view.update(cx, |view, cx| {
                        view.set_tree_diff(this.tree_diff.as_ref(), cx);
                    });
                }

                if !matches!(this.active_view, ActiveView::ReviewThread) && file_count > 0 {
                    this.show_file_list(cx);
                }
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn show_file_list(&mut self, cx: &mut Context<Self>) {
        let file_list = cx.new(|cx| {
            FileList::new(
                self.base_branch.clone(),
                self.head_branch.clone(),
                self.tree_diff.as_ref(),
                cx,
            )
        });
        let subscription = cx.subscribe(&file_list, |this, _file_list, event, cx| match event {
            FileListEvent::OpenFileDiff(path) => {
                this.open_file_diff(path.clone(), cx);
            }
            FileListEvent::OpenLocalFile(path) => {
                this.open_local_file_by_path(path.clone(), cx);
            }
        });
        self.file_list = Some((file_list, subscription));
        self.active_view = ActiveView::FileList;
        cx.notify();
    }

    fn show_pull_request_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pull_request_list.is_none() {
            let pr_list = cx.new(|cx| {
                PullRequestList::new(
                    self.provider.clone(),
                    self.remote_owner.clone(),
                    self.remote_repo.clone(),
                    window,
                    cx,
                )
            });
            let subscription =
                cx.subscribe(&pr_list, |this, _pr_list, event, cx| match event {
                    PullRequestListEvent::Selected(pr) => {
                        this.select_pull_request(&pr.clone(), cx);
                    }
                });
            self.pull_request_list = Some((pr_list, subscription));
        }

        if let Some((pr_list, _)) = &self.pull_request_list {
            pr_list.update(cx, |list, cx| list.load_if_empty(cx));
        }

        self.active_view = ActiveView::PullRequestList;
        cx.notify();
    }

    fn open_file_diff(&mut self, path: RepoPath, cx: &mut Context<Self>) {
        if let Some((file_list, _)) = &self.file_list {
            file_list.update(cx, |fl, cx| {
                fl.mark_viewed(path.clone(), cx);
                fl.select_path(&path, cx);
            });
        }

        self.pending_action = Some(PendingAction::OpenDiff(path));
        cx.notify();
    }

    fn open_local_file_by_path(&mut self, path: RepoPath, cx: &mut Context<Self>) {
        self.pending_action = Some(PendingAction::OpenLocal(path));
        cx.notify();
    }

    fn flush_pending_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(action) = self.pending_action.take() else {
            return;
        };
        match action {
            PendingAction::OpenDiff(path) => {
                let Some(workspace) = self._workspace.upgrade() else {
                    return;
                };
                let Some(active_repo) = self.active_repository.as_ref() else {
                    return;
                };
                let Some(project_path) =
                    active_repo.read(cx).repo_path_to_project_path(&path, cx)
                else {
                    return;
                };

                if let Some(pr) = self.selected_pr.as_ref() {
                    let base_ref = pr.base_ref.clone();
                    let head_ref = Some(pr.head_sha.clone());
                    workspace.update(cx, |workspace, cx| {
                        git_ui::project_diff::ProjectDiff::deploy_merge_diff(
                            workspace,
                            base_ref,
                            head_ref,
                            Some(project_path),
                            window,
                            cx,
                        );
                    });
                } else {
                    window.dispatch_action(Box::new(git_ui::project_diff::BranchDiff), cx);
                }
            }
            PendingAction::OpenLocal(path) => {
                let Some(active_repo) = self.active_repository.as_ref() else {
                    return;
                };
                let Some(project_path) =
                    active_repo.read(cx).repo_path_to_project_path(&path, cx)
                else {
                    return;
                };
                let Some(workspace) = self._workspace.upgrade() else {
                    return;
                };
                workspace.update(cx, |workspace, cx| {
                    workspace
                        .open_path_preview(project_path, None, true, false, true, window, cx)
                        .detach_and_log_err(cx);
                });
            }
            PendingAction::SelectPullRequest(pr) => {
                self.create_review_view(&pr, window, cx);
            }
        }
    }
}

impl Render for ReviewPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.flush_pending_action(window, cx);
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
                ActiveView::PullRequestList => {
                    if let Some((pr_list, _)) = &self.pull_request_list {
                        parent.child(pr_list.clone())
                    } else {
                        parent.child(
                            v_flex()
                                .size_full()
                                .justify_center()
                                .items_center()
                                .child(Label::new("Loading...").color(Color::Muted)),
                        )
                    }
                }
                ActiveView::ReviewThread => {
                    if let Some((review_view, _)) = &self.review_view {
                        parent.child(review_view.clone())
                    } else {
                        parent.child(
                            v_flex()
                                .size_full()
                                .justify_center()
                                .items_center()
                                .child(Label::new("Loading...").color(Color::Muted)),
                        )
                    }
                }
                ActiveView::FileList => {
                    if let Some((file_list, _)) = &self.file_list {
                        parent.child(file_list.clone())
                    } else {
                        parent.child(
                            v_flex()
                                .size_full()
                                .justify_center()
                                .items_center()
                                .child(Label::new("Loading...").color(Color::Muted)),
                        )
                    }
                }
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

fn parse_github_remote(url: &str) -> anyhow::Result<(String, String)> {
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        let rest = rest.trim_end_matches(".git");
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() == 2 {
            return Ok((parts[0].to_string(), parts[1].to_string()));
        }
    }

    if let Some(rest) = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
    {
        let rest = rest.trim_end_matches(".git");
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() == 2 {
            return Ok((parts[0].to_string(), parts[1].to_string()));
        }
    }

    anyhow::bail!("Could not parse GitHub owner/repo from remote URL: {}", url)
}
