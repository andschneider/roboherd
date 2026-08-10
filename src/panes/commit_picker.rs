use std::collections::BTreeSet;
use std::ops::RangeInclusive;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::widgets::{Cell, Paragraph, Row, Table, TableState};

use crate::git::Commit;
use crate::roborev::{ReviewType, Selection};

/// What a keypress asks the picker to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// The picker handled the key itself.
    Handled,
    Quit,
    Enqueue,
}

/// Which list the picker is showing. The agent list takes the whole body rather than sharing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Commits,
    Agents,
}

/// Where the picker is in its one-shot lifecycle. It browses, enqueues once per chosen agent, then
/// reports what roborev said and waits to be dismissed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Browsing,
    /// An enqueue that was refused before it ran, with the picker still usable behind it.
    Refused(String),
    /// How many enqueues have finished, out of how many were asked for.
    Enqueuing(usize, usize),
    /// One line per enqueue: roborev's own reply, or the error that enqueue hit. Shown verbatim
    /// rather than parsed.
    Reported(Vec<String>),
}

/// The picker's state, kept free of terminal handles so key handling and selection are testable
/// without a pty.
pub(crate) struct Picker {
    commits: CommitPicker,
    agents: AgentPicker,
    view: View,
    phase: Phase,
    /// The reviewer prompt every enqueue runs under, cycled by the type key.
    review_type: ReviewType,
}

/// Commit list state and range selection.
struct CommitPicker {
    commits: Vec<Commit>,
    ages: Vec<String>,
    /// How many paths carry uncommitted changes, or `None` for a clean tree. `Some` pins a row
    /// above the log that reviews the working tree rather than any commit.
    dirty: Option<usize>,
    /// The checked-out branch, empty on a detached HEAD. The footer names it while the dirty row is
    /// selected, since a worktree is the reason to be reviewing uncommitted changes at all.
    branch: String,
    /// Index of the highlighted row, counting the dirty row when there is one.
    cursor: usize,
    /// The other end of a marked range, set by the mark key.
    anchor: Option<usize>,
    /// Scroll position, owned by the table widget.
    table: TableState,
}

/// Agent list state and selection.
struct AgentPicker {
    /// Installed agents, sorted.
    agents: Vec<String>,
    /// Row and display label for roborev's configured default.
    default: Option<(usize, String)>,
    /// Agent names to enqueue with. Empty leaves roborev to choose.
    chosen: BTreeSet<String>,
    /// Whether the checked default still represents roborev's implicit choice.
    implicit: bool,
    cursor: usize,
    /// Selection saved when the agent list opens, restored when escape cancels.
    saved: BTreeSet<String>,
    saved_implicit: bool,
    /// Scroll position, owned by the table widget.
    table: TableState,
}

/// How far the page keys jump.
const PAGE: usize = 10;

/// A commit's age in exactly four columns: the count, its unit letter, then padding, as in `12h `
/// or `3d  `.
///
/// The minute, hour, day, week, and year ladder keeps the count within two digits and caps it at
/// `99y `. Unlike git's localized `%ar` prose, the fixed width keeps the subject column stable.
fn age(timestamp: i64, now: i64) -> String {
    let minutes = now.saturating_sub(timestamp).max(0) / 60;
    let (hours, days) = (minutes / 60, minutes / 60 / 24);

    let (unit, count) = if minutes < 60 {
        ('m', minutes)
    } else if hours < 24 {
        ('h', hours)
    } else if days < 7 {
        ('d', days)
    } else if days < 365 {
        ('w', days / 7)
    } else {
        ('y', days / 365)
    };

    let count = count.min(99);
    match count < 10 {
        true => format!("{count}{unit}  "),
        false => format!("{count}{unit} "),
    }
}

impl Picker {
    pub(crate) fn new(
        commits: Vec<Commit>,
        dirty: usize,
        branch: String,
        agents: Vec<String>,
        default_agent: Option<String>,
        now: i64,
    ) -> Self {
        Self {
            commits: CommitPicker::new(commits, dirty, branch, now),
            agents: AgentPicker::new(agents, default_agent),
            view: View::Commits,
            phase: Phase::Browsing,
            review_type: ReviewType::Default,
        }
    }

    /// Apply a keypress and report what it asked for.
    pub(crate) fn on_key(&mut self, key: KeyEvent) -> Action {
        // A refusal is a note about the last enter, not a mode. The next key clears it, and the
        // caller sets a fresh one if that key was another enter on a range git still disagrees with.
        if matches!(self.phase, Phase::Refused(_)) {
            self.phase = Phase::Browsing;
        }

        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Action::Quit;
        }

        match self.view {
            View::Commits if key.code == KeyCode::Char('a') => {
                self.agents.begin_edit();
                self.view = View::Agents;
                Action::Handled
            }
            View::Commits if key.code == KeyCode::Char('t') => {
                self.review_type = self.review_type.next();
                Action::Handled
            }
            View::Commits => self.commits.on_key(key),
            View::Agents => {
                match key.code {
                    KeyCode::Esc => self.agents.cancel_edit(),
                    KeyCode::Enter | KeyCode::Char('a') | KeyCode::Char('q') => {}
                    _ => return self.agents.on_key(key),
                }
                self.view = View::Commits;
                Action::Handled
            }
        }
    }

    pub(crate) fn selection(&self) -> Option<Selection> {
        self.commits.selection()
    }

    pub(crate) fn marked_shas(&self) -> Vec<&str> {
        self.commits
            .marked()
            .filter_map(|row| self.commits.commit_at(row))
            .map(|commit| commit.sha.as_str())
            .collect()
    }

    pub(crate) fn refuse(&mut self, reason: String) {
        self.phase = Phase::Refused(reason);
    }

    pub(crate) fn take_agents(&mut self) -> Vec<Option<String>> {
        self.agents.take_chosen_agents()
    }

    pub(crate) fn review_type(&self) -> ReviewType {
        self.review_type
    }

    pub(crate) fn set_enqueuing(&mut self, done: usize, total: usize) {
        self.phase = Phase::Enqueuing(done, total);
    }

    pub(crate) fn set_reported(&mut self, replies: Vec<String>) {
        self.phase = Phase::Reported(replies);
    }
}

impl CommitPicker {
    fn new(commits: Vec<Commit>, dirty: usize, branch: String, now: i64) -> Self {
        let ages = commits
            .iter()
            .map(|commit| age(commit.timestamp, now))
            .collect();
        Self {
            commits,
            ages,
            dirty: (dirty > 0).then_some(dirty),
            branch,
            cursor: 0,
            anchor: None,
            table: TableState::new(),
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Char('q') => Action::Quit,
            // Escape clears a marked range before closing the picker.
            KeyCode::Esc => match self.anchor.take() {
                Some(_) => Action::Handled,
                None => Action::Quit,
            },
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::PageUp => self.move_cursor(-(PAGE as isize)),
            KeyCode::PageDown => self.move_cursor(PAGE as isize),
            KeyCode::Home | KeyCode::Char('g') => self.jump(0),
            KeyCode::End | KeyCode::Char('G') => self.jump(self.rows().saturating_sub(1)),
            KeyCode::Char('v') | KeyCode::Char(' ') => self.toggle_mark(),
            KeyCode::Enter if self.rows() > 0 => Action::Enqueue,
            _ => Action::Handled,
        }
    }

    /// How many rows the list holds, the dirty row included.
    fn rows(&self) -> usize {
        self.commits.len() + self.offset()
    }

    /// How far the dirty row pushes the log down, so row 0 is the working tree when it is dirty.
    fn offset(&self) -> usize {
        usize::from(self.dirty.is_some())
    }

    /// The commit a row shows, or `None` for the dirty row.
    fn commit_at(&self, row: usize) -> Option<&Commit> {
        self.commits.get(row.checked_sub(self.offset())?)
    }

    /// What the footer calls the working tree's branch. A detached HEAD has no name to give.
    fn branch_label(&self) -> &str {
        match self.branch.is_empty() {
            true => "detached",
            false => &self.branch,
        }
    }

    fn move_cursor(&mut self, delta: isize) -> Action {
        let target = self.cursor.saturating_add_signed(delta);
        self.jump(target)
    }

    /// Move the cursor to `target`, holding it inside the log while a range is marked.
    ///
    /// A range is `START^..END` between two commits, which the working tree has no place in, so a
    /// mark floors the cursor below the dirty row rather than letting a span swallow it.
    fn jump(&mut self, target: usize) -> Action {
        let floor = match self.anchor {
            Some(_) => self.offset(),
            None => 0,
        };
        self.cursor = target.clamp(floor, self.rows().saturating_sub(1));
        Action::Handled
    }

    /// Start a range at the cursor, or clear one already marked. The dirty row is reviewed alone,
    /// so it marks nothing.
    fn toggle_mark(&mut self) -> Action {
        if self.commits.is_empty() || self.commit_at(self.cursor).is_none() {
            return Action::Handled;
        }
        self.anchor = match self.anchor {
            Some(_) => None,
            None => Some(self.cursor),
        };
        Action::Handled
    }

    /// The rows a selection covers, or the cursor when nothing is marked.
    fn marked(&self) -> RangeInclusive<usize> {
        match self.anchor {
            Some(anchor) => anchor.min(self.cursor)..=anchor.max(self.cursor),
            None => self.cursor..=self.cursor,
        }
    }

    /// What an enqueue would review, or `None` when the list is empty.
    ///
    /// `roborev review START END` reviews `START^..END` inclusive, so START is the older end. `git
    /// log` lists newest first, which makes the older end the *higher* index. Passing them in row
    /// order would invert the range.
    fn selection(&self) -> Option<Selection> {
        if self.rows() == 0 {
            return None;
        }
        let marked = self.marked();
        let (newest, oldest) = (*marked.start(), *marked.end());

        // Only the dirty row has no commit behind it, and a mark can never reach it.
        let Some(end) = self.commit_at(newest) else {
            return Some(Selection::Dirty);
        };

        if oldest == newest {
            return Some(Selection::Commit(end.sha.clone()));
        }
        match self.commit_at(oldest) {
            Some(start) => Some(Selection::Range(start.sha.clone(), end.sha.clone())),
            None => Some(Selection::Commit(end.sha.clone())),
        }
    }

    /// The dirty row's columns, aligned with a commit's.
    fn dirty_row(&self) -> Option<[String; 6]> {
        let count = self.dirty?;
        let files = match count {
            1 => "1 file".to_string(),
            count => format!("{count} files"),
        };
        Some([
            String::new(),
            "dirty".to_string(),
            String::new(),
            format!("uncommitted changes ({files})"),
            String::new(),
            String::new(),
        ])
    }

    fn render(&mut self, frame: &mut Frame, body: Rect) {
        let marked = self.marked();
        let dirty = self.dirty_row();
        let commits = self.commits.iter().zip(&self.ages).map(|(commit, age)| {
            [
                "",
                commit.short_sha.as_str(),
                age.as_str(),
                commit.subject.as_str(),
                commit.author.as_str(),
                "",
            ]
            .map(str::to_string)
        });

        let rows = dirty
            .into_iter()
            .chain(commits)
            .enumerate()
            .map(|(index, cells)| {
                let row = Row::new(cells);
                match self.anchor.is_some() && marked.contains(&index) {
                    true => row.style(Style::new().reversed()),
                    false => row,
                }
            });

        let table = Table::new(
            rows,
            [
                Constraint::Length(0),
                Constraint::Length(8),
                Constraint::Length(4),
                Constraint::Min(20),
                Constraint::Length(16),
                Constraint::Length(0),
            ],
        )
        .row_highlight_style(Style::new().reversed());

        self.table.select(Some(self.cursor));
        frame.render_stateful_widget(table, body, &mut self.table);
    }
}

impl AgentPicker {
    fn new(agents: Vec<String>, default_agent: Option<String>) -> Self {
        let default = default_agent.and_then(|default_agent| {
            let row = agents.iter().position(|agent| agent == &default_agent)?;
            Some((row, default_agent))
        });
        let (default, chosen) = match default {
            Some((row, agent)) => {
                let label = format!("{agent} (default)");
                (Some((row, label)), BTreeSet::from([agent]))
            }
            None => (None, BTreeSet::new()),
        };
        Self {
            agents,
            default,
            chosen,
            implicit: true,
            cursor: 0,
            saved: BTreeSet::new(),
            saved_implicit: true,
            table: TableState::new(),
        }
    }

    fn begin_edit(&mut self) {
        self.saved.clone_from(&self.chosen);
        self.saved_implicit = self.implicit;
    }

    fn cancel_edit(&mut self) {
        self.chosen.clone_from(&self.saved);
        self.implicit = self.saved_implicit;
    }

    fn on_key(&mut self, key: KeyEvent) -> Action {
        let last = self.agents.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.cursor = self.cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1).min(last);
            }
            KeyCode::Char(' ') => self.toggle(),
            _ => {}
        }
        Action::Handled
    }

    /// Add the agent under the cursor to the selection, or take it back out.
    fn toggle(&mut self) {
        let Some(agent) = self.agents.get(self.cursor) else {
            return;
        };

        self.implicit = false;
        if !self.chosen.remove(agent) {
            self.chosen.insert(agent.clone());
        }
    }

    /// Take the agents to enqueue with in roster order.
    fn take_chosen_agents(&mut self) -> Vec<Option<String>> {
        if self.implicit || self.chosen.is_empty() {
            return vec![None];
        }
        std::mem::take(&mut self.chosen)
            .into_iter()
            .map(Some)
            .collect()
    }

    #[cfg(test)]
    fn chosen_agents(&self) -> Vec<Option<&str>> {
        if self.implicit {
            return vec![None];
        }
        let agents: Vec<Option<&str>> = self
            .chosen
            .iter()
            .map(|agent| Some(agent.as_str()))
            .collect();

        match agents.is_empty() {
            true => vec![None],
            false => agents,
        }
    }

    fn summary(&self) -> String {
        let mut summary = String::new();
        for agent in &self.chosen {
            if !summary.is_empty() {
                summary.push_str(", ");
            }
            summary.push_str(agent);
        }

        match summary.is_empty() {
            true => "roborev's choice".to_string(),
            false => summary,
        }
    }

    fn render(&mut self, frame: &mut Frame, body: Rect) {
        let rows = self.agents.iter().enumerate().map(|(index, agent)| {
            let mark = match self.chosen.contains(agent) {
                true => "[X]",
                false => "[ ]",
            };
            let label = match &self.default {
                Some((row, label)) if *row == index => Cell::from(label.as_str()),
                _ => Cell::from(agent.as_str()),
            };
            Row::new([Cell::from(""), Cell::from(mark), label, Cell::from("")])
        });

        let table = Table::new(
            rows,
            [
                Constraint::Length(0),
                Constraint::Length(3),
                Constraint::Min(10),
                Constraint::Length(0),
            ],
        )
        .row_highlight_style(Style::new().reversed());

        self.table
            .select((!self.agents.is_empty()).then_some(self.cursor));
        frame.render_stateful_widget(table, body, &mut self.table);
    }
}

impl Picker {
    pub(crate) fn render(&mut self, frame: &mut Frame) {
        let [body, footer] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());

        // Replies replace the lists because several results will not fit in the footer.
        if let Phase::Reported(replies) = &self.phase {
            frame.render_widget(Paragraph::new(replies.join("\n")), body);
            frame.render_widget(Paragraph::new(self.status()).dim(), footer);
            return;
        }

        match self.view {
            View::Commits => self.commits.render(frame, body),
            View::Agents => self.agents.render(frame, body),
        }
        frame.render_widget(Paragraph::new(self.status()).dim(), footer);
    }

    /// The footer line: key hints while browsing, progress while enqueuing, a dismissal prompt once
    /// roborev has replied.
    fn status(&self) -> String {
        const SEPARATOR: char = '|';
        match &self.phase {
            Phase::Refused(reason) => reason.clone(),
            Phase::Enqueuing(done, total) => format!("enqueuing {}/{total}...", done + 1),
            Phase::Reported(replies) => match replies.len() {
                1 => "press any key to close".to_string(),
                count => format!("{count} results {SEPARATOR} press any key to close"),
            },
            Phase::Browsing if self.view == View::Agents => {
                format!(" esc {SEPARATOR} space toggles {SEPARATOR} enter")
            }
            Phase::Browsing if self.commits.rows() == 0 => {
                format!("nothing to review in this checkout {SEPARATOR} q to close")
            }
            Phase::Browsing => {
                let marked = self.commits.marked();
                let span = marked.end() - marked.start() + 1;
                let selection = match self.commits.anchor {
                    Some(_) => format!("{span} commits marked {SEPARATOR} esc clears"),
                    // The working tree is reviewed alone, so a range key would do nothing here.
                    // The row already says what is selected, leaving the footer to say where.
                    None if self.commits.commit_at(self.commits.cursor).is_none() => {
                        self.commits.branch_label().to_string()
                    }
                    None => "v range".to_string(),
                };
                format!(
                    " {selection} {SEPARATOR} a {} {SEPARATOR} t {} {SEPARATOR} enter",
                    self.agents.summary(),
                    self.review_type.label()
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::style::Modifier;

    use super::{Action, Phase, Picker, View, age};
    use crate::commands::pick_commit::refusal;
    use crate::git;
    use crate::git::Commit;
    use crate::git::fixtures::{repo_with_an_interleaved_branch, repo_with_one_commit};
    use crate::roborev::{ReviewType, Selection};

    /// A fixed "now" so ages render the same on every machine.
    const NOW: i64 = 1_754_006_400;

    /// Tall enough to fit the fixture list and hint bar. Not tied to the popup's manifest
    /// height, which is a config value and not a promise about the pane's actual size.
    const TEST_HEIGHT: u16 = 23;

    const TEST_WIDTH: u16 = 80;

    /// A fixed roster, sorted as `roborev::installed_agents` returns it.
    fn agent_roster() -> Vec<String> {
        ["claude-code", "codex", "pi"]
            .iter()
            .map(|agent| agent.to_string())
            .collect()
    }

    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;

    /// Newest first, as `git log` lists them.
    fn picker() -> Picker {
        picker_with_dirty(0)
    }

    /// The same log, over a working tree carrying `dirty` changed paths.
    fn picker_with_dirty(dirty: usize) -> Picker {
        let commits = ["newest", "middle", "oldest"]
            .into_iter()
            .enumerate()
            .map(|(index, subject)| Commit {
                sha: format!("{index}").repeat(40),
                short_sha: format!("{index}").repeat(7),
                author: "Ada".to_string(),
                timestamp: NOW - (index as i64 + 1) * DAY,
                subject: subject.to_string(),
            })
            .collect();
        Picker::new(
            commits,
            dirty,
            "main".to_string(),
            agent_roster(),
            Some("codex".to_string()),
            NOW,
        )
    }

    fn press(picker: &mut Picker, code: KeyCode) -> Action {
        picker.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// Everything the picker drew, as one string. Cells are row-major, so a substring check finds
    /// text within a line without depending on where the line falls.
    fn rendered(picker: &mut Picker, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal
            .draw(|frame| picker.render(frame))
            .expect("the picker renders");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    /// Render at popup size and assert on what came out. Every `needle` must appear and every one
    /// prefixed `!` must not, with the whole frame reported when one disagrees.
    fn shows(picker: &mut Picker, needles: &[&str]) {
        let frame = rendered(picker, TEST_WIDTH, TEST_HEIGHT);
        for needle in needles {
            match needle.strip_prefix('!') {
                Some(absent) => assert!(!frame.contains(absent), "{absent:?} in {frame:?}"),
                None => assert!(frame.contains(needle), "{needle:?} not in {frame:?}"),
            }
        }
    }

    fn selected_row_has_highlighted_padding(picker: &mut Picker) {
        let mut terminal =
            Terminal::new(TestBackend::new(TEST_WIDTH, TEST_HEIGHT)).expect("test terminal");
        terminal
            .draw(|frame| picker.render(frame))
            .expect("the picker renders");
        let buffer = terminal.backend().buffer();
        for x in [0, 79] {
            let cell = buffer.cell((x, 0)).expect("edge is in the buffer");
            assert_eq!(cell.symbol(), " ");
            assert!(cell.modifier.contains(Modifier::REVERSED));
        }
    }

    /// A picker over a real repository's log, for the checks that need git to answer.
    fn picker_in(dir: &Path) -> Picker {
        let commits = git::recent_commits(dir, 10).expect("log runs");
        Picker::new(
            commits,
            0,
            "main".to_string(),
            agent_roster(),
            Some("codex".to_string()),
            NOW,
        )
    }

    /// The commit a selection names, for the tests that only care which row was taken.
    fn commit(picker: &Picker) -> Option<Selection> {
        picker.commits.selection()
    }

    #[test]
    fn an_unmarked_picker_enqueues_the_commit_under_the_cursor() {
        let mut picker = picker();
        press(&mut picker, KeyCode::Down);
        assert_eq!(commit(&picker), Some(Selection::Commit("1".repeat(40))));
    }

    #[test]
    fn a_range_passes_the_older_commit_first_whichever_way_it_was_marked() {
        // roborev wants START END and START is the older end, while `git log` puts the older end
        // further down. So argv is row order reversed, and marking direction cannot matter.
        for ends in [[KeyCode::Home, KeyCode::End], [KeyCode::End, KeyCode::Home]] {
            let mut picker = picker();
            press(&mut picker, ends[0]);
            press(&mut picker, KeyCode::Char('v'));
            press(&mut picker, ends[1]);
            assert_eq!(
                commit(&picker),
                Some(Selection::Range("2".repeat(40), "0".repeat(40)))
            );
        }
    }

    #[test]
    fn a_range_collapsed_onto_one_row_enqueues_a_single_commit() {
        let mut picker = picker();
        press(&mut picker, KeyCode::Char('v'));
        assert_eq!(commit(&picker), Some(Selection::Commit("0".repeat(40))));
    }

    #[test]
    fn escape_clears_a_mark_before_it_closes_the_picker() {
        let mut picker = picker();
        press(&mut picker, KeyCode::Char('v'));
        press(&mut picker, KeyCode::Down);

        assert_eq!(press(&mut picker, KeyCode::Esc), Action::Handled);
        assert_eq!(commit(&picker), Some(Selection::Commit("1".repeat(40))));
        assert_eq!(press(&mut picker, KeyCode::Esc), Action::Quit);
    }

    #[test]
    fn the_cursor_stops_at_both_ends() {
        let mut picker = picker();
        for _ in 0..10 {
            press(&mut picker, KeyCode::Up);
        }
        assert_eq!(picker.commits.cursor, 0);

        for _ in 0..10 {
            press(&mut picker, KeyCode::Down);
        }
        assert_eq!(picker.commits.cursor, 2);
    }

    #[test]
    fn ctrl_c_quits() {
        let mut picker = picker();
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(picker.on_key(key), Action::Quit);
    }

    #[test]
    fn the_commit_list_and_the_key_hints_render() {
        // sha, age, subject, author. The sha column is a column wider than an abbreviated sha, so
        // it pads before the age.
        shows(
            &mut picker(),
            &[
                "0000000  1d   newest",
                "Ada",
                "v range | a codex | t default | enter",
                "!> ",
            ],
        );
    }

    #[test]
    fn the_commit_list_leaves_space_after_the_author() {
        let mut picker = picker();
        picker.commits.commits[0].author = "1234567890abcdef".to_string();
        let frame = rendered(&mut picker, TEST_WIDTH, TEST_HEIGHT);
        assert!(frame[..TEST_WIDTH as usize].ends_with("1234567890abcdef "));
    }

    #[test]
    fn selected_rows_highlight_both_padding_columns() {
        let mut picker = picker();
        selected_row_has_highlighted_padding(&mut picker);
        press(&mut picker, KeyCode::Char('a'));
        selected_row_has_highlighted_padding(&mut picker);
    }

    #[test]
    fn every_commit_in_a_marked_range_uses_the_terminal_highlight() {
        let mut picker = picker();
        press(&mut picker, KeyCode::Char('v'));
        press(&mut picker, KeyCode::End);

        let mut terminal =
            Terminal::new(TestBackend::new(TEST_WIDTH, TEST_HEIGHT)).expect("test terminal");
        terminal
            .draw(|frame| picker.render(frame))
            .expect("the picker renders");
        for y in 0..3 {
            let cell = terminal
                .backend()
                .buffer()
                .cell((0, y))
                .expect("row is in the buffer");
            assert!(cell.modifier.contains(Modifier::REVERSED));
        }
    }

    #[test]
    fn a_pane_too_short_for_the_list_still_renders() {
        // A pane listing is not a promise about size, and a layout that panicked on a short pane
        // would take the whole popup down.
        let mut picker = picker();
        assert!(!rendered(&mut picker, 40, 1).is_empty());
    }

    /// Move the cursor to `row`, marking from wherever it started.
    fn mark_through(picker: &mut Picker, from: usize, to: usize) {
        picker.commits.cursor = from;
        press(picker, KeyCode::Char('v'));
        picker.commits.cursor = to;
    }

    #[test]
    fn a_span_is_refused_when_git_disagrees_with_it() {
        // The fixture is `merge, main2, side1, main1, base` by date. Rows 1 to 3 highlight side1,
        // which is unreachable from main2, so that range reviews two of the three marked. Rows 0
        // to 3 are an ancestry range and match. Row 4 is the root, which has no `START^`.
        for (from, to, want) in [
            (1, 3, Some("skips 1 marked")),
            (0, 3, None),
            (0, 4, Some("root commit")),
        ] {
            let dir = repo_with_an_interleaved_branch();
            let mut picker = picker_in(dir.path());
            mark_through(&mut picker, from, to);

            let reason = refusal(dir.path(), &picker).expect("git answers");
            match want {
                Some(fragment) => assert!(
                    reason.as_deref().is_some_and(|r| r.contains(fragment)),
                    "rows {from}..{to}: {reason:?}"
                ),
                None => assert_eq!(reason, None, "rows {from}..{to}"),
            }
        }
    }

    #[test]
    fn a_lone_commit_needs_no_range_check_even_at_the_root() {
        // One revision is not a range, so nothing asks git for a `START^` the root does not have,
        // whether the row was marked or merely under the cursor.
        let dir = repo_with_one_commit();
        let mut picker = picker_in(dir.path());
        assert_eq!(refusal(dir.path(), &picker).expect("no git call"), None);

        picker.commits.anchor = Some(0);
        assert_eq!(refusal(dir.path(), &picker).expect("no git call"), None);
    }

    #[test]
    fn a_range_failing_for_any_other_reason_surfaces_the_error() {
        let dir = repo_with_an_interleaved_branch();
        let mut picker = picker_in(dir.path());
        mark_through(&mut picker, 0, 3);

        // Neither end is the root, so a rev-list failure here is not the root-commit case.
        // Dropping the object store stands in for corruption or a missing object.
        std::fs::remove_dir_all(dir.path().join(".git/objects")).expect("drop the object store");
        assert!(
            refusal(dir.path(), &picker).is_err(),
            "a broken repository was reported as a root-commit range"
        );
    }

    #[test]
    fn a_refusal_shows_in_the_footer_and_the_next_key_clears_it() {
        let mut picker = picker();
        picker.phase = Phase::Refused("range covers 2 unmarked".to_string());
        shows(&mut picker, &["range covers 2 unmarked"]);

        press(&mut picker, KeyCode::Down);
        assert_eq!(picker.phase, Phase::Browsing);
    }

    #[test]
    fn the_default_agent_starts_as_roborevs_implicit_choice() {
        let mut picker = picker();
        assert_eq!(picker.agents.take_chosen_agents(), vec![None]);
    }

    #[test]
    fn each_chosen_agent_becomes_its_own_enqueue_in_roster_order() {
        let mut picker = picker();
        picker.view = View::Agents;
        picker.agents.cursor = 2;
        press(&mut picker, KeyCode::Char(' ')); // pi, out of order
        picker.agents.cursor = 0;
        press(&mut picker, KeyCode::Char(' ')); // then claude-code

        assert_eq!(
            picker.agents.take_chosen_agents(),
            vec![
                Some("claude-code".to_string()),
                Some("codex".to_string()),
                Some("pi".to_string())
            ]
        );
    }

    #[test]
    fn an_empty_selection_hands_the_choice_back_to_roborev() {
        // Deselecting every explicit choice hands the decision back to roborev.
        let mut picker = picker();
        press(&mut picker, KeyCode::Char('a'));
        press(&mut picker, KeyCode::Down); // onto codex
        press(&mut picker, KeyCode::Char(' '));
        assert_eq!(picker.agents.chosen_agents(), vec![None]);

        let mut bare = Picker::new(
            picker.commits.commits,
            0,
            "main".to_string(),
            Vec::new(),
            Some("codex".to_string()),
            NOW,
        );
        press(&mut bare, KeyCode::Char('a'));
        press(&mut bare, KeyCode::Char(' '));
        assert_eq!(bare.agents.chosen_agents(), vec![None]);
    }

    #[test]
    fn the_agent_list_only_ever_returns_to_the_commits() {
        // Agent-list keys return rather than enqueueing or closing the picker.
        for key in [KeyCode::Enter, KeyCode::Esc, KeyCode::Char('a')] {
            let mut picker = picker();
            press(&mut picker, KeyCode::Char('a'));
            assert_eq!(picker.view, View::Agents);
            assert_eq!(press(&mut picker, key), Action::Handled);
            assert_eq!(picker.view, View::Commits, "{key:?} did not return");
        }

        let mut picker = picker();
        press(&mut picker, KeyCode::Char('a'));
        let interrupt = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(picker.on_key(interrupt), Action::Quit);
    }

    #[test]
    fn escape_discards_agent_selection_changes() {
        let mut picker = picker();
        press(&mut picker, KeyCode::Char('a'));
        press(&mut picker, KeyCode::Char(' '));
        press(&mut picker, KeyCode::Enter);
        assert_eq!(
            picker.agents.chosen_agents(),
            vec![Some("claude-code"), Some("codex")]
        );

        press(&mut picker, KeyCode::Char('a'));
        press(&mut picker, KeyCode::Char(' '));
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Char(' '));
        press(&mut picker, KeyCode::Esc);
        assert_eq!(
            picker.agents.chosen_agents(),
            vec![Some("claude-code"), Some("codex")]
        );
    }

    #[test]
    fn the_agent_list_labels_the_default_and_the_footer_follows_the_selection() {
        let mut picker = picker();
        press(&mut picker, KeyCode::Char('a'));
        shows(
            &mut picker,
            &[
                "[X] codex (default)",
                "[ ] claude-code",
                "esc | space toggles | enter",
                "!> ",
            ],
        );

        press(&mut picker, KeyCode::Char(' ')); // select claude-code
        press(&mut picker, KeyCode::Enter);
        shows(&mut picker, &["a claude-code, codex"]);
    }

    #[test]
    fn an_unresolved_default_marks_no_row() {
        let mut picker = Picker::new(
            picker().commits.commits,
            0,
            "main".to_string(),
            agent_roster(),
            None,
            NOW,
        );
        press(&mut picker, KeyCode::Char('a'));
        shows(&mut picker, &["!(default)", "![X]"]);
    }

    #[test]
    fn replies_render_as_lines_and_take_over_from_the_key_hints() {
        let mut picker = picker();
        picker.phase = Phase::Reported(vec!["Enqueued job 42".to_string()]);
        shows(
            &mut picker,
            &["Enqueued job 42", "press any key", "!v range"],
        );

        picker.phase = Phase::Reported(vec![
            "Enqueued job 42".to_string(),
            "Enqueued job 43".to_string(),
        ]);
        shows(
            &mut picker,
            &["Enqueued job 42", "Enqueued job 43", "2 results"],
        );
    }

    #[test]
    fn every_age_is_exactly_four_columns() {
        for offset in [
            0,
            30,
            MINUTE,
            59 * MINUTE,
            HOUR,
            23 * HOUR,
            DAY,
            6 * DAY,
            7 * DAY,
            364 * DAY,
            365 * DAY,
            120 * 365 * DAY,
        ] {
            let rendered = age(NOW - offset, NOW);
            assert_eq!(
                rendered.chars().count(),
                4,
                "{rendered:?} is not four columns"
            );
        }
    }

    #[test]
    fn ages_step_through_the_unit_ladder() {
        assert_eq!(age(NOW - 30, NOW), "0m  ");
        assert_eq!(age(NOW - 12 * MINUTE, NOW), "12m ");
        assert_eq!(age(NOW - 12 * HOUR, NOW), "12h ");
        assert_eq!(age(NOW - DAY, NOW), "1d  ");
        assert_eq!(age(NOW - 6 * DAY, NOW), "6d  ");
        assert_eq!(age(NOW - 7 * DAY, NOW), "1w  ");
        assert_eq!(age(NOW - 70 * DAY, NOW), "10w ");
        assert_eq!(age(NOW - 364 * DAY, NOW), "52w ");
        assert_eq!(age(NOW - 365 * DAY, NOW), "1y  ");
    }

    #[test]
    fn a_commit_dated_in_the_future_ages_to_zero() {
        // Clock skew across machines is ordinary, and a negative age would blow past four columns.
        assert_eq!(age(NOW + 5 * DAY, NOW), "0m  ");
    }

    #[test]
    fn an_empty_checkout_has_nothing_to_enqueue() {
        let mut picker = Picker::new(
            Vec::new(),
            0,
            "main".to_string(),
            agent_roster(),
            Some("codex".to_string()),
            NOW,
        );
        assert_eq!(press(&mut picker, KeyCode::Enter), Action::Handled);
        assert_eq!(press(&mut picker, KeyCode::Char('v')), Action::Handled);
        assert_eq!(commit(&picker), None);
    }

    #[test]
    fn the_dirty_row_shifts_the_log_and_stays_out_of_every_range() {
        let mut picker = picker_with_dirty(3);
        // The row says what is selected, so the footer says which working tree it belongs to.
        shows(
            &mut picker,
            &["dirty", "uncommitted changes (3 files)", " main | a codex"],
        );
        assert_eq!(commit(&picker), Some(Selection::Dirty));

        picker.commits.branch = String::new();
        shows(&mut picker, &["detached | a codex"]);

        // `START^..END` is defined between commits, so the mark key does nothing here.
        assert_eq!(press(&mut picker, KeyCode::Char('v')), Action::Handled);
        assert_eq!(picker.commits.anchor, None);

        // Marking from the oldest commit and running back to the top stops above the dirty row,
        // which also puts the log's own first row at index 1.
        press(&mut picker, KeyCode::End);
        press(&mut picker, KeyCode::Char('v'));
        press(&mut picker, KeyCode::Home);
        assert_eq!(picker.commits.cursor, 1);
        assert_eq!(
            commit(&picker),
            Some(Selection::Range("2".repeat(40), "0".repeat(40)))
        );

        press(&mut picker, KeyCode::Char('t'));
        assert_eq!(picker.review_type, ReviewType::Security);
    }
}
