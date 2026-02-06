use collections::HashSet;
use git::repository::RepoPath;
use git::status::{TreeDiff, TreeDiffStatus};
use gpui::{Context, EventEmitter, Render, SharedString, Window};
use ui::{Color, Icon, IconName, IconSize, IntoElement, Label, LabelSize, div, h_flex, prelude::*, v_flex};
use zed_actions::review_panel::OpenLocalFile;

pub enum FileListEvent {
    OpenFileDiff(RepoPath),
    OpenLocalFile(RepoPath),
}

pub struct FileList {
    base_branch: Option<SharedString>,
    head_branch: Option<SharedString>,
    entries: Vec<(RepoPath, TreeDiffStatus)>,
    viewed_files: HashSet<RepoPath>,
    selected_entry: Option<usize>,
}

impl EventEmitter<FileListEvent> for FileList {}

fn sort_diff_entries(tree_diff: &TreeDiff) -> Vec<(RepoPath, TreeDiffStatus)> {
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

impl FileList {
    pub fn new(
        base_branch: Option<SharedString>,
        head_branch: Option<SharedString>,
        tree_diff: Option<&TreeDiff>,
        _cx: &mut Context<Self>,
    ) -> Self {
        let entries = tree_diff.map(sort_diff_entries).unwrap_or_default();
        Self {
            base_branch,
            head_branch,
            entries,
            viewed_files: HashSet::default(),
            selected_entry: None,
        }
    }

    pub fn set_diff(
        &mut self,
        base: Option<SharedString>,
        head: Option<SharedString>,
        diff: Option<&TreeDiff>,
        cx: &mut Context<Self>,
    ) {
        self.base_branch = base;
        self.head_branch = head;
        self.entries = diff.map(sort_diff_entries).unwrap_or_default();
        self.viewed_files.clear();
        self.selected_entry = None;
        cx.notify();
    }

    pub fn mark_viewed(&mut self, path: RepoPath, cx: &mut Context<Self>) {
        self.viewed_files.insert(path);
        cx.notify();
    }

    fn select_next(&mut self, _: &menu::SelectNext, _window: &mut Window, cx: &mut Context<Self>) {
        let count = self.entries.len();
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
        let count = self.entries.len();
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

    fn confirm(&mut self, _: &menu::Confirm, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected) = self.selected_entry else {
            return;
        };
        if let Some((path, _)) = self.entries.get(selected) {
            cx.emit(FileListEvent::OpenFileDiff(path.clone()));
        }
    }

    fn open_local_file(
        &mut self,
        _: &OpenLocalFile,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(selected) = self.selected_entry else {
            return;
        };
        let Some((path, status)) = self.entries.get(selected) else {
            return;
        };
        if matches!(status, TreeDiffStatus::Deleted { .. }) {
            return;
        }
        cx.emit(FileListEvent::OpenLocalFile(path.clone()));
    }

    pub fn select_path(&mut self, path: &RepoPath, cx: &mut Context<Self>) {
        self.selected_entry = self.entries.iter().position(|(p, _)| p == path);
        cx.notify();
    }
}

impl Render for FileList {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.entries.is_empty() {
            return v_flex()
                .size_full()
                .justify_center()
                .items_center()
                .child(Label::new("Loading...").color(Color::Muted))
                .into_any_element();
        }

        let header_text = format!(
            "{} <- {}",
            self.base_branch.as_ref().map(|s| s.as_ref()).unwrap_or("?"),
            self.head_branch.as_ref().map(|s| s.as_ref()).unwrap_or("?"),
        );

        let file_count = self.entries.len();
        let added = self.entries
            .iter()
            .filter(|(_, s)| matches!(s, TreeDiffStatus::Added))
            .count();
        let modified = self.entries
            .iter()
            .filter(|(_, s)| matches!(s, TreeDiffStatus::Modified { .. }))
            .count();
        let deleted = self.entries
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
            .on_action(cx.listener(Self::open_local_file))
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
            .children(self.entries.iter().enumerate().map(|(ix, (path, status))| {
                let (icon, color) = match status {
                    TreeDiffStatus::Added => (IconName::Plus, Color::Created),
                    TreeDiffStatus::Modified { .. } => (IconName::Pencil, Color::Modified),
                    TreeDiffStatus::Deleted { .. } => (IconName::Dash, Color::Deleted),
                };

                let is_selected = self.selected_entry == Some(ix);
                let is_viewed = self.viewed_files.contains(path);
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

                let path = path.clone();
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
                        cx.listener(move |this, _event, _window, cx| {
                            this.viewed_files.insert(path.clone());
                            this.selected_entry = this.entries.iter().position(|(p, _)| *p == path);
                            cx.emit(FileListEvent::OpenFileDiff(path.clone()));
                            cx.notify();
                        })
                    })
            }))
            .into_any_element()
    }
}
