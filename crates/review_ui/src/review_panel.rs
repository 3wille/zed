use crate::comment_card::CommentCard;
use crate::file_list::{FileList, FileListEvent};
use crate::github_provider::GitHubProvider;
use crate::pull_request_list::{PullRequestList, PullRequestListEvent};
use crate::github_token::resolve_github_token;
use crate::review_panel_settings::ReviewPanelSettings;
use crate::review_provider::{
    FileChangeStatus, PullRequestFile, PullRequestInfo, PullRequestState, ReviewComment,
    ReviewProvider, ReviewStatus,
};
use anyhow::Result;
use collections::HashMap;
use credentials_provider::CredentialsProvider;
use editor::Editor;
use fs::Fs;
use git::repository::RepoPath;
use git::status::{DiffTreeType, TreeDiff, TreeDiffStatus};
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
    ButtonLike, ButtonSize, Color, ContextMenu, DynamicSpacing, ElevationIndex, Icon, IconButton,
    IconName, IconSize, IntoElement, Label, LabelSize, PopoverMenu, PopoverMenuHandle, SplitButton,
    Tab, Tooltip, h_flex, prelude::*, v_flex,
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

enum PendingFileAction {
    OpenDiff(RepoPath),
    OpenLocal(RepoPath),
}

pub struct ReviewPanel {
    _workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    active_repository: Option<Entity<Repository>>,
    base_branch: Option<SharedString>,
    head_branch: Option<SharedString>,
    tree_diff: Option<TreeDiff>,
    file_list: Option<(Entity<FileList>, Subscription)>,
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
    pr_comments: Vec<ReviewComment>,
    pr_comments_loading: bool,
    pr_api_files: Vec<PullRequestFile>,
    comment_editor: Entity<Editor>,
    comment_submitting: bool,
    review_action: ReviewStatus,
    review_action_menu_handle: PopoverMenuHandle<ContextMenu>,
    pr_ref_fetch_task: Option<gpui::Task<Result<()>>>,
    pending_file_action: Option<PendingFileAction>,
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

        let comment_editor = cx.new(|cx| {
            let mut editor = Editor::auto_height(3, 6, window, cx);
            editor.set_placeholder_text("Leave a comment…", window, cx);
            editor.set_show_gutter(false, cx);
            editor.set_show_wrap_guides(false, cx);
            editor.set_show_indent_guides(false, cx);
            editor.set_use_autoclose(false);
            editor
        });

        let mut this = Self {
            _workspace: weak_workspace,
            project,
            active_repository,
            base_branch: None,
            head_branch: None,
            tree_diff: None,
            file_list: None,
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
            pr_comments: Vec::new(),
            pr_comments_loading: false,
            pr_api_files: Vec::new(),
            comment_editor,
            comment_submitting: false,
            review_action: ReviewStatus::Commented,
            review_action_menu_handle: PopoverMenuHandle::default(),
            pr_ref_fetch_task: None,
            pending_file_action: None,
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
                    menu.entry("Configuration", None, |_window, _cx| {
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

        self.load_pr_comments(pr.number, cx);
        self.load_pr_api_files(pr.number, cx);
        self.fetch_pr_ref(pr.number, cx);
        self.set_active_view(ActiveView::ReviewThread, cx);
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

    fn load_pr_comments(&mut self, pr_number: u32, cx: &mut Context<Self>) {
        let Some(provider) = self.provider.clone() else {
            return;
        };
        let Some(owner) = self.remote_owner.clone() else {
            return;
        };
        let Some(repo) = self.remote_repo.clone() else {
            return;
        };

        self.pr_comments_loading = true;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let comments = provider.fetch_reviews(&owner, &repo, pr_number).await?;
            this.update(cx, |this, cx| {
                this.pr_comments = comments;
                this.pr_comments_loading = false;
                cx.notify();
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn load_pr_api_files(&mut self, pr_number: u32, cx: &mut Context<Self>) {
        let Some(provider) = self.provider.clone() else {
            return;
        };
        let Some(owner) = self.remote_owner.clone() else {
            return;
        };
        let Some(repo) = self.remote_repo.clone() else {
            return;
        };

        cx.spawn(async move |this, cx| {
            let files = provider
                .fetch_pull_request_files(&owner, &repo, pr_number)
                .await?;
            this.update(cx, |this, cx| {
                this.pr_api_files = files;
                cx.notify();
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn submit_review_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(provider) = self.provider.clone() else {
            return;
        };
        let Some(owner) = self.remote_owner.clone() else {
            return;
        };
        let Some(repo) = self.remote_repo.clone() else {
            return;
        };
        let Some(pr) = &self.selected_pr else {
            return;
        };

        let body = self.comment_editor.read(cx).text(cx).to_string();
        let action = self.review_action.clone();
        let pr_number = pr.number;
        self.comment_submitting = true;
        cx.notify();

        match action {
            ReviewStatus::Commented => {
                if body.trim().is_empty() {
                    self.comment_submitting = false;
                    return;
                }
                cx.spawn_in(window, async move |this, cx| {
                    let new_comment = provider
                        .submit_comment(&owner, &repo, pr_number, &body, None, None)
                        .await?;
                    this.update_in(cx, |this, window, cx| {
                        this.pr_comments.push(new_comment);
                        this.comment_submitting = false;
                        this.comment_editor.update(cx, |editor, cx| {
                            editor.clear(window, cx);
                        });
                        cx.notify();
                    })?;
                    anyhow::Ok(())
                })
                .detach_and_log_err(cx);
            }
            ReviewStatus::Approved | ReviewStatus::ChangesRequested => {
                let body_opt = if body.trim().is_empty() {
                    None
                } else {
                    Some(body)
                };
                cx.spawn_in(window, async move |this, cx| {
                    provider
                        .submit_review(&owner, &repo, pr_number, action, body_opt.as_deref())
                        .await?;
                    this.update_in(cx, |this, window, cx| {
                        this.comment_submitting = false;
                        this.comment_editor.update(cx, |editor, cx| {
                            editor.clear(window, cx);
                        });
                        this.load_pr_comments(pr_number, cx);
                        cx.notify();
                    })?;
                    anyhow::Ok(())
                })
                .detach_and_log_err(cx);
            }
            ReviewStatus::Pending => {}
        }
    }

    fn review_action_label(&self) -> &'static str {
        match &self.review_action {
            ReviewStatus::Commented => "Comment",
            ReviewStatus::Approved => "Approve",
            ReviewStatus::ChangesRequested => "Request Changes",
            ReviewStatus::Pending => "Comment",
        }
    }

    fn render_review_action_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = self.review_action_label();
        let is_submitting = self.comment_submitting;

        let label_color = if is_submitting {
            Color::Disabled
        } else {
            Color::Default
        };

        SplitButton::new(
            ButtonLike::new_rounded_left("review-submit-left")
                .layer(ElevationIndex::ModalSurface)
                .size(ButtonSize::Compact)
                .disabled(is_submitting)
                .child(Label::new(label).size(LabelSize::Small).color(label_color))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.submit_review_action(window, cx);
                })),
            self.render_review_action_menu(cx).into_any_element(),
        )
    }

    fn render_review_action_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let weak_panel = cx.weak_entity();
        let current = self.review_action.clone();

        PopoverMenu::new("review-action-menu")
            .trigger(
                ButtonLike::new_rounded_right("review-action-menu-trigger")
                    .layer(ElevationIndex::ModalSurface)
                    .size(ButtonSize::None)
                    .child(
                        h_flex()
                            .px_1()
                            .h_full()
                            .justify_center()
                            .border_l_1()
                            .border_color(cx.theme().colors().border)
                            .child(Icon::new(IconName::ChevronDown).size(IconSize::XSmall)),
                    ),
            )
            .with_handle(self.review_action_menu_handle.clone())
            .anchor(Corner::TopRight)
            .menu(move |window, cx| {
                let weak_panel = weak_panel.clone();
                let current = current.clone();
                Some(ContextMenu::build(window, cx, move |menu, _window, _cx| {
                    menu.toggleable_entry(
                        "Comment",
                        matches!(current, ReviewStatus::Commented),
                        IconPosition::Start,
                        None,
                        {
                            let weak_panel = weak_panel.clone();
                            move |_window, cx| {
                                weak_panel
                                    .update(cx, |this, cx| {
                                        this.review_action = ReviewStatus::Commented;
                                        cx.notify();
                                    })
                                    .ok();
                            }
                        },
                    )
                    .toggleable_entry(
                        "Approve",
                        matches!(current, ReviewStatus::Approved),
                        IconPosition::Start,
                        None,
                        {
                            let weak_panel = weak_panel.clone();
                            move |_window, cx| {
                                weak_panel
                                    .update(cx, |this, cx| {
                                        this.review_action = ReviewStatus::Approved;
                                        cx.notify();
                                    })
                                    .ok();
                            }
                        },
                    )
                    .toggleable_entry(
                        "Request Changes",
                        matches!(current, ReviewStatus::ChangesRequested),
                        IconPosition::Start,
                        None,
                        {
                            let weak_panel = weak_panel.clone();
                            move |_window, cx| {
                                weak_panel
                                    .update(cx, |this, cx| {
                                        this.review_action = ReviewStatus::ChangesRequested;
                                        cx.notify();
                                    })
                                    .ok();
                            }
                        },
                    )
                }))
            })
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

                this.show_file_list(cx);
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

        self.pending_file_action = Some(PendingFileAction::OpenDiff(path));
        cx.notify();
    }

    fn open_local_file_by_path(&mut self, path: RepoPath, cx: &mut Context<Self>) {
        self.pending_file_action = Some(PendingFileAction::OpenLocal(path));
        cx.notify();
    }

    fn flush_pending_file_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(action) = self.pending_file_action.take() else {
            return;
        };
        match action {
            PendingFileAction::OpenDiff(path) => {
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
            PendingFileAction::OpenLocal(path) => {
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
        }
    }
    fn render_review_thread(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(pr) = &self.selected_pr else {
            return v_flex()
                .size_full()
                .justify_center()
                .items_center()
                .child(Label::new("No PR selected").color(Color::Muted))
                .into_any_element();
        };

        let pr_number = pr.number;
        let pr_title = pr.title.clone();
        let pr_author = pr.author.clone();
        let file_count = self.pr_api_files.len();

        // Group comments by file path
        let mut file_comments: HashMap<SharedString, Vec<ReviewComment>> = HashMap::default();
        let mut general_comments: Vec<ReviewComment> = Vec::new();
        for comment in &self.pr_comments {
            if let Some(path) = &comment.path {
                file_comments
                    .entry(path.clone())
                    .or_default()
                    .push(comment.clone());
            } else {
                general_comments.push(comment.clone());
            }
        }

        // Build the file list — prefer API files, fall back to local diff entries
        let file_entries: Vec<(SharedString, Option<FileChangeStatus>, u32, u32)> =
            if !self.pr_api_files.is_empty() {
                self.pr_api_files
                    .iter()
                    .map(|f| {
                        (
                            f.path.clone(),
                            Some(f.status.clone()),
                            f.additions,
                            f.deletions,
                        )
                    })
                    .collect()
            } else if let Some(tree_diff) = &self.tree_diff {
                let mut entries: Vec<_> = tree_diff.entries.iter().collect();
                entries.sort_by(|(a, _), (b, _)| a.cmp(b));
                entries
                    .into_iter()
                    .map(|(path, status)| {
                        let file_status = match status {
                            TreeDiffStatus::Added => Some(FileChangeStatus::Added),
                            TreeDiffStatus::Modified { .. } => Some(FileChangeStatus::Modified),
                            TreeDiffStatus::Deleted { .. } => Some(FileChangeStatus::Deleted),
                        };
                        (
                            SharedString::from(path.as_std_path().to_string_lossy().to_string()),
                            file_status,
                            0,
                            0,
                        )
                    })
                    .collect()
            } else {
                Vec::new()
            };

        // Scrollable content
        let mut scrollable = v_flex()
            .id("review-scrollable")
            .flex_1()
            .overflow_scroll()
            .gap_1();

        // File entries with inline comments
        for (path, status, additions, deletions) in &file_entries {
            let (icon, color) = match status {
                Some(FileChangeStatus::Added) => (IconName::Plus, Color::Created),
                Some(FileChangeStatus::Modified) => (IconName::Pencil, Color::Modified),
                Some(FileChangeStatus::Deleted) => (IconName::Dash, Color::Deleted),
                Some(FileChangeStatus::Renamed { .. }) => (IconName::ArrowRight, Color::Modified),
                None => (IconName::File, Color::Muted),
            };

            let repo_path = RepoPath::new(path.as_ref()).ok();
            let file_row = h_flex()
                .id(SharedString::from(format!("pr_file_{}", path)))
                .px_2()
                .py_1()
                .gap_2()
                .rounded_md()
                .cursor_pointer()
                .hover(|style| style.bg(cx.theme().colors().ghost_element_hover))
                .child(Icon::new(icon).size(IconSize::Small).color(color))
                .child(
                    h_flex()
                        .flex_1()
                        .overflow_x_hidden()
                        .gap_2()
                        .child(
                            Label::new(path.to_string())
                                .size(LabelSize::Small)
                                .single_line(),
                        )
                        .when(*additions > 0, |el| {
                            el.child(
                                Label::new(format!("+{}", additions))
                                    .size(LabelSize::XSmall)
                                    .color(Color::Created),
                            )
                        })
                        .when(*deletions > 0, |el| {
                            el.child(
                                Label::new(format!("-{}", deletions))
                                    .size(LabelSize::XSmall)
                                    .color(Color::Deleted),
                            )
                        }),
                );

            let file_row = if let Some(repo_path) = repo_path {
                file_row.on_click(cx.listener(move |this, _event, _window, cx| {
                    this.open_file_diff(repo_path.clone(), cx);
                }))
            } else {
                file_row
            };

            scrollable = scrollable.child(file_row);

            // Inline comments for this file
            if let Some(comments) = file_comments.get(path) {
                for comment in comments {
                    scrollable = scrollable.child(CommentCard::new(comment.clone()));
                }
            }
        }

        // General comments section
        if !general_comments.is_empty() {
            scrollable = scrollable.child(
                h_flex().px_2().pt_2().child(
                    Label::new(format!("General comments ({})", general_comments.len()))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                ),
            );
            for comment in &general_comments {
                scrollable = scrollable.child(CommentCard::new(comment.clone()));
            }
        }

        // Loading indicator for comments
        if self.pr_comments_loading {
            scrollable = scrollable.child(
                h_flex().px_2().py_1().child(
                    Label::new("Loading comments...")
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                ),
            );
        }

        v_flex()
            .id("review-thread")
            .size_full()
            // PR header (fixed top)
            .child(
                h_flex()
                    .flex_none()
                    .px_2()
                    .py_1()
                    .gap_1()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        IconButton::new("back-to-pr-list", IconName::ArrowLeft)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Back to PR list"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.selected_pr = None;
                                this.pr_comments.clear();
                                this.pr_api_files.clear();
                                this.show_pull_request_list(window, cx);
                            })),
                    )
                    .child(
                        Label::new(format!("#{}", pr_number))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        div().overflow_x_hidden().flex_1().child(
                            Label::new(pr_title.to_string())
                                .size(LabelSize::Small)
                                .single_line(),
                        ),
                    )
                    .child(
                        Label::new(format!("by {} · {} files", pr_author, file_count))
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    ),
            )
            // Scrollable content (files + inline comments + general comments)
            .child(scrollable)
            // Fixed bottom: comment editor + review action button
            .child(
                v_flex()
                    .flex_none()
                    .border_t_1()
                    .border_color(cx.theme().colors().border)
                    .bg(cx.theme().colors().editor_background)
                    .child(
                        div()
                            .id("comment-editor-container")
                            .px_2()
                            .pt_2()
                            .w_full()
                            .cursor_text()
                            .on_click(cx.listener(|this, _, window, cx| {
                                window.focus(&this.comment_editor.focus_handle(cx), cx);
                            }))
                            .child(self.comment_editor.clone()),
                    )
                    .child(
                        h_flex()
                            .px_2()
                            .py_1()
                            .justify_end()
                            .child(self.render_review_action_button(cx)),
                    ),
            )
            .into_any_element()
    }


}

impl Render for ReviewPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.flush_pending_file_action(window, cx);
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
                ActiveView::ReviewThread => parent.child(self.render_review_thread(cx)),
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
    // Handle SSH: git@github.com:owner/repo.git
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        let rest = rest.trim_end_matches(".git");
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() == 2 {
            return Ok((parts[0].to_string(), parts[1].to_string()));
        }
    }

    // Handle HTTPS: https://github.com/owner/repo.git
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
