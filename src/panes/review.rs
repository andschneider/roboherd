use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::widgets::{Block, Paragraph, Wrap};
use serde::Deserialize;

use crate::roborev;

/// Which surface owns the pane body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Reading,
    Commenting,
}

/// What one input asks the command loop to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    Handled,
    Quit,
    Refresh,
    Close,
    SubmitComment(String),
    Show(i64),
    Copy,
}

/// Review and editor state kept free of process handles.
pub(crate) struct ReviewView {
    job_id: Option<i64>,
    /// Reviewable jobs newest first, which is the order roborev lists them in.
    jobs: Vec<i64>,
    position: Option<usize>,
    review: String,
    scroll: u16,
    max_scroll: u16,
    page_len: u16,
    mode: Mode,
    draft: String,
    notice: Option<String>,
    closed: bool,
}

/// Token counters embedded as JSON inside the show response.
#[derive(Default, Deserialize)]
#[serde(default)]
struct TokenUsage {
    input_tokens: i64,
    cached_input_tokens: i64,
    cache_creation_tokens: i64,
    total_output_tokens: i64,
    peak_context_tokens: i64,
    cost_usd: f64,
    has_cost: bool,
}

/// Maximum comment size passed through one argv entry.
const COMMENT_LIMIT: usize = 16 * 1024;

fn review_text(review: &roborev::ShownReview) -> String {
    let title = match review.job.as_ref().map(|job| short_ref(&job.git_ref)) {
        Some(reference) if !reference.is_empty() => format!(
            "Review for {reference} (job {}, by {})",
            review.job_id, review.agent
        ),
        _ => format!("Review for job {} (by {})", review.job_id, review.agent),
    };
    let tokens = review
        .job
        .as_ref()
        .and_then(|job| token_summary(&job.token_usage))
        .map(|summary| format!("Tokens: {summary}\n\n"))
        .unwrap_or_default();
    // Preserve leading indentation for indented code blocks.
    let output = match review.output.trim().is_empty() {
        true => "_No review output for this job._",
        false => review.output.trim_end(),
    };
    let mut text = format!("# {title}\n\n{tokens}{output}\n");

    if !review.comments.is_empty() {
        text.push_str("\n## Comments\n");
        for comment in &review.comments {
            let timestamp = match comment.created_at.is_empty() {
                true => String::new(),
                false => format!(" · {}", comment.created_at),
            };
            text.push_str(&format!(
                "\n**{}**{timestamp}\n\n{}\n",
                comment.responder,
                comment.response.trim_end()
            ));
        }
    }

    text
}

fn short_ref(reference: &str) -> &str {
    match reference.len() >= 7 && reference.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        true => &reference[..7],
        false => reference,
    }
}

fn token_summary(raw: &str) -> Option<String> {
    let usage: TokenUsage = serde_json::from_str(raw).ok()?;
    let (input, input_label) = match usage.peak_context_tokens {
        0 => (usage.input_tokens, "in"),
        tokens => (tokens, "ctx"),
    };
    let has_tokens = input != 0
        || usage.total_output_tokens != 0
        || usage.cached_input_tokens != 0
        || usage.cache_creation_tokens != 0;
    if !has_tokens {
        return usage.has_cost.then(|| format!("~${:.2}", usage.cost_usd));
    }

    let mut notes = Vec::new();
    if usage.cached_input_tokens != 0 {
        notes.push(format!(
            "{} cached",
            format_token_count(usage.cached_input_tokens)
        ));
    }
    if usage.cache_creation_tokens != 0 {
        notes.push(format!(
            "{} written",
            format_token_count(usage.cache_creation_tokens)
        ));
    }
    let notes = match notes.is_empty() {
        true => String::new(),
        false => format!(" ({})", notes.join(", ")),
    };
    let cost = match usage.has_cost {
        true => format!(" · ~${:.2}", usage.cost_usd),
        false => String::new(),
    };
    Some(format!(
        "{} {input_label}{notes} · {} out{cost}",
        format_token_count(input),
        format_token_count(usage.total_output_tokens)
    ))
}

fn format_token_count(tokens: i64) -> String {
    match tokens {
        1_000_000.. => format!("{:.1}M", tokens as f64 / 1_000_000.0),
        1_000.. => format!("{:.1}k", tokens as f64 / 1_000.0),
        _ => tokens.to_string(),
    }
}

impl ReviewView {
    pub(crate) fn from_review(
        job_id: Option<i64>,
        jobs: Vec<i64>,
        review: &roborev::ShownReview,
    ) -> Self {
        Self::new(
            job_id,
            jobs,
            review_text(review),
            review.closed.unwrap_or(false),
        )
    }

    fn new(job_id: Option<i64>, jobs: Vec<i64>, review: String, closed: bool) -> Self {
        let position = job_id.and_then(|id| jobs.iter().position(|job| *job == id));
        Self {
            job_id,
            jobs,
            position,
            review,
            scroll: 0,
            max_scroll: 0,
            page_len: 1,
            mode: Mode::Reading,
            draft: String::new(),
            notice: None,
            closed,
        }
    }

    pub(crate) fn job_id(&self) -> Option<i64> {
        self.job_id
    }

    /// The displayed review text, exactly as rendered, for the copy-to-clipboard action.
    pub(crate) fn review_text(&self) -> &str {
        &self.review
    }

    pub(crate) fn replace_review(&mut self, review: &roborev::ShownReview, notice: String) {
        self.load(review);
        self.notice = Some(notice);
    }

    /// Show a different job, which only happens once its review has been fetched.
    pub(crate) fn show_job(&mut self, job_id: i64, review: &roborev::ShownReview) {
        self.job_id = Some(job_id);
        self.position = self.jobs.iter().position(|job| *job == job_id);
        self.load(review);
        self.notice = None;
    }

    fn load(&mut self, review: &roborev::ShownReview) {
        self.review = review_text(review);
        self.closed = review.closed.unwrap_or(false);
        self.scroll = 0;
    }

    /// Replace the steppable jobs, which a refresh refetches alongside the review.
    pub(crate) fn set_jobs(&mut self, jobs: Vec<i64>) {
        self.position = self
            .job_id
            .and_then(|id| jobs.iter().position(|job| *job == id));
        self.jobs = jobs;
    }

    pub(crate) fn set_notice(&mut self, notice: String) {
        self.notice = Some(notice);
    }

    /// Whether a notice is showing, which is what gives the command loop a deadline to wait on.
    pub(crate) fn has_notice(&self) -> bool {
        self.notice.is_some()
    }

    pub(crate) fn clear_notice(&mut self) {
        self.notice = None;
    }

    pub(crate) fn mark_closed(&mut self, notice: String) {
        self.closed = true;
        self.notice = Some(notice);
    }

    pub(crate) fn finish_comment(&mut self) {
        self.mode = Mode::Reading;
        self.draft.clear();
    }

    pub(crate) fn on_key(&mut self, key: KeyEvent) -> Action {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Action::Quit;
        }

        match self.mode {
            Mode::Reading => self.on_reading_key(key),
            Mode::Commenting => self.on_comment_key(key),
        }
    }

    fn on_reading_key(&mut self, key: KeyEvent) -> Action {
        self.notice = None;
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
            KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                Action::Handled
            }
            KeyCode::Down => {
                self.scroll = self.scroll.saturating_add(1).min(self.max_scroll);
                Action::Handled
            }
            KeyCode::Char('j') => self.step(1),
            KeyCode::Char('k') => self.step(-1),
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(self.page_len);
                Action::Handled
            }
            KeyCode::PageDown => {
                self.scroll = self
                    .scroll
                    .saturating_add(self.page_len)
                    .min(self.max_scroll);
                Action::Handled
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.scroll = 0;
                Action::Handled
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.scroll = self.max_scroll;
                Action::Handled
            }
            KeyCode::Char('r') => Action::Refresh,
            KeyCode::Char('y') => Action::Copy,
            KeyCode::Char('a') if self.job_id.is_some() && !self.closed => Action::Close,
            KeyCode::Char('c') if self.job_id.is_some() => {
                self.mode = Mode::Commenting;
                Action::Handled
            }
            _ => Action::Handled,
        }
    }

    /// Move `offset` places through the job list, which runs newest first.
    ///
    /// A displayed job absent from the list has nowhere to step from, so it says nothing.
    fn step(&mut self, offset: isize) -> Action {
        let Some(position) = self.position else {
            return Action::Handled;
        };

        match position
            .checked_add_signed(offset)
            .and_then(|next| self.jobs.get(next))
        {
            Some(job_id) => Action::Show(*job_id),
            None if self.jobs.len() < 2 => Action::Handled,
            None => {
                let edge = match offset > 0 {
                    true => "oldest review",
                    false => "newest review",
                };
                self.notice = Some(edge.to_string());
                Action::Handled
            }
        }
    }

    fn on_comment_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Reading;
                self.draft.clear();
                self.notice = None;
                Action::Handled
            }
            KeyCode::Enter if self.draft.trim().is_empty() => {
                // An empty positional makes roborev open $EDITOR.
                self.notice = Some("comment is empty".to_string());
                Action::Handled
            }
            KeyCode::Enter => Action::SubmitComment(self.draft.clone()),
            KeyCode::Backspace | KeyCode::Delete => {
                self.draft.pop();
                self.notice = None;
                Action::Handled
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.append("\n");
                Action::Handled
            }
            KeyCode::Tab => {
                self.append("\t");
                Action::Handled
            }
            KeyCode::Char(ch)
                if key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.append(&ch.to_string());
                Action::Handled
            }
            _ => Action::Handled,
        }
    }

    pub(crate) fn on_paste(&mut self, text: &str) -> Action {
        if self.mode == Mode::Commenting {
            self.append(&text.replace("\r\n", "\n").replace('\r', "\n"));
        }
        Action::Handled
    }

    fn append(&mut self, text: &str) {
        self.notice = None;
        let mut full = false;
        for ch in text
            .chars()
            .filter(|ch| matches!(ch, '\n' | '\t') || !ch.is_control())
        {
            if self.draft.len() + ch.len_utf8() > COMMENT_LIMIT {
                full = true;
                break;
            }
            self.draft.push(ch);
        }
        if full {
            self.notice = Some(format!("comment limit is {COMMENT_LIMIT} bytes"));
        }
    }

    pub(crate) fn render(&mut self, frame: &mut Frame) {
        // A notice gets its own line above the key hints, but only when one is showing, so it
        // never steals a row from the body on an ordinary frame.
        let notice = self.notice.clone();
        let areas = match notice {
            Some(_) => Layout::vertical([
                Constraint::Min(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(frame.area()),
            None => {
                Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(frame.area())
            }
        };
        let body = areas[0];

        match self.mode {
            Mode::Reading => self.render_review(frame, body),
            Mode::Commenting => self.render_comment(frame, body),
        }

        frame.render_widget(Paragraph::new(self.footer()).dim(), areas[areas.len() - 1]);
        if let Some(notice) = notice {
            frame.render_widget(Paragraph::new(notice).dim(), areas[1]);
        }
    }

    fn render_review(&mut self, frame: &mut Frame, body: Rect) {
        let paragraph =
            Paragraph::new(tui_markdown::from_str(&self.review)).wrap(Wrap { trim: false });
        self.page_len = body.height.max(1);
        self.max_scroll = paragraph
            .line_count(body.width)
            .saturating_sub(body.height as usize)
            .min(u16::MAX as usize) as u16;
        self.scroll = self.scroll.min(self.max_scroll);
        frame.render_widget(paragraph.scroll((self.scroll, 0)), body);
    }

    fn render_comment(&self, frame: &mut Frame, body: Rect) {
        let text = match self.draft.is_empty() {
            true => "Type your comment...▏".to_string(),
            false => format!("{}▏", self.draft.replace('\t', "    ")),
        };
        // Measure inside the border so wrapped drafts keep the cursor in the box.
        let block = Block::bordered().title(" Add comment ");
        let width = block.inner(body).width;
        let paragraph = Paragraph::new(text).block(block).wrap(Wrap { trim: false });
        let scroll = paragraph
            .line_count(width)
            .saturating_sub(body.height as usize)
            .min(u16::MAX as usize) as u16;
        frame.render_widget(paragraph.scroll((scroll, 0)), body);
    }

    /// The key hints line. A notice, when set, renders on its own line above this one.
    fn footer(&self) -> String {
        match self.mode {
            Mode::Reading => self.reading_footer(),
            Mode::Commenting => format!(
                "{}/{} bytes | ctrl+j newline | enter submit | esc cancel",
                self.draft.len(),
                COMMENT_LIMIT
            ),
        }
    }

    fn reading_footer(&self) -> String {
        let mut parts = Vec::new();
        if self.closed {
            parts.push("closed");
        }
        if self.jobs.len() > 1 {
            parts.push("jk review");
        }
        parts.push("↑↓ scroll");
        parts.push("r refresh");
        if self.job_id.is_some() && !self.closed {
            parts.push("a close");
        }
        if self.job_id.is_some() {
            parts.push("c comment");
        }
        parts.push("y copy");
        parts.push("q exit");

        parts.join(" | ")
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::style::Modifier;

    use crate::roborev::{ShownComment, ShownJob, ShownReview};

    use super::{Action, Mode, ReviewView, review_text};

    fn press(view: &mut ReviewView, code: KeyCode) -> Action {
        view.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn reading_actions_are_explicit_and_quit_is_read_only() {
        let mut view = ReviewView::new(Some(42), Vec::new(), "review".to_string(), false);

        assert_eq!(press(&mut view, KeyCode::Char('q')), Action::Quit);
        assert_eq!(press(&mut view, KeyCode::Char('r')), Action::Refresh);
        assert_eq!(press(&mut view, KeyCode::Char('y')), Action::Copy);
        assert_eq!(press(&mut view, KeyCode::Char('a')), Action::Close);
        assert_eq!(press(&mut view, KeyCode::Char('c')), Action::Handled);
        assert_eq!(view.mode, Mode::Commenting);
    }

    /// Copy is not gated on a displayed job, unlike close and comment.
    #[test]
    fn copy_works_without_a_job_id() {
        let mut view = ReviewView::new(None, Vec::new(), "review".to_string(), false);

        assert_eq!(press(&mut view, KeyCode::Char('y')), Action::Copy);
        assert_eq!(view.review_text(), "review");
    }

    /// Jobs run newest first, so `j` walks toward older reviews and `k` toward newer ones.
    #[test]
    fn stepping_walks_the_job_list_and_stops_at_each_end() {
        let jobs = vec![9, 8, 7];
        let mut newest = ReviewView::new(Some(9), jobs.clone(), "review".to_string(), false);
        let mut oldest = ReviewView::new(Some(7), jobs, "review".to_string(), false);

        assert_eq!(press(&mut newest, KeyCode::Char('j')), Action::Show(8));
        assert_eq!(press(&mut newest, KeyCode::Char('k')), Action::Handled);
        assert_eq!(newest.notice.as_deref(), Some("newest review"));

        assert_eq!(press(&mut oldest, KeyCode::Char('k')), Action::Show(8));
        assert_eq!(press(&mut oldest, KeyCode::Char('j')), Action::Handled);
        assert_eq!(oldest.notice.as_deref(), Some("oldest review"));

        // The notice sits on its own line, so the key hints stay unchanged while it shows.
        assert!(oldest.footer().contains("q exit"), "{}", oldest.footer());
    }

    /// The title already names the job, so the footer stays worth reading while stepping.
    #[test]
    fn arriving_at_a_review_leaves_the_footer_as_help() {
        let arrived = ShownReview {
            job_id: 8,
            agent: "codex".to_string(),
            output: "## Review Findings".to_string(),
            closed: Some(false),
            job: None,
            comments: Vec::new(),
        };
        let mut view = ReviewView::new(Some(9), vec![9, 8], "review".to_string(), false);
        view.set_notice("stale notice".to_string());

        view.show_job(8, &arrived);

        assert_eq!(view.job_id(), Some(8));
        assert!(view.footer().contains("jk review"), "{}", view.footer());
    }

    #[test]
    fn the_footer_offers_stepping_only_when_another_review_exists() {
        let alone = ReviewView::new(Some(9), vec![9], "review".to_string(), false);
        let among = ReviewView::new(Some(9), vec![9, 8], "review".to_string(), false);

        assert!(!alone.footer().contains("jk review"), "{}", alone.footer());
        assert!(among.footer().contains("jk review"), "{}", among.footer());
    }

    #[test]
    fn comment_input_handles_unicode_newlines_and_paste_as_text() {
        let mut view = ReviewView::new(Some(42), Vec::new(), "review".to_string(), false);
        press(&mut view, KeyCode::Char('c'));
        assert_eq!(press(&mut view, KeyCode::Enter), Action::Handled);
        view.on_paste("known\r\n世界\u{1b}");
        press(&mut view, KeyCode::Backspace);
        view.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));

        assert_eq!(view.draft, "known\n世\n");
        assert_eq!(
            press(&mut view, KeyCode::Enter),
            Action::SubmitComment("known\n世\n".to_string())
        );
    }

    #[test]
    fn json_review_keeps_the_human_header_and_comments() {
        let review = ShownReview {
            job_id: 388,
            agent: "codex".to_string(),
            output: "## Review Findings".to_string(),
            closed: Some(false),
            job: Some(ShownJob {
                git_ref: "3882160de38f0ee5b39c38cfbc23ab652fc0554c".to_string(),
                token_usage: r#"{"input_tokens":484969,"cached_input_tokens":419328,"total_output_tokens":5467}"#.to_string(),
            }),
            comments: vec![ShownComment {
                responder: "andrew".to_string(),
                response: "looks good".to_string(),
                created_at: "2026-08-02T00:12:44-07:00".to_string(),
            }],
        };

        let text = review_text(&review);

        assert!(text.starts_with(
            "# Review for 3882160 (job 388, by codex)\n\n\
             Tokens: 485.0k in (419.3k cached) · 5.5k out\n\n\
             ## Review Findings"
        ));
        assert!(text.contains("\n## Comments\n"));
        assert!(text.contains("**andrew** · 2026-08-02T00:12:44-07:00\n\nlooks good"));
    }

    #[test]
    fn an_indented_code_block_keeps_the_indentation_that_makes_it_one() {
        let review = ShownReview {
            job_id: 1,
            agent: "codex".to_string(),
            output: "    fn main() {}\n\nProse after.  \n\n".to_string(),
            closed: None,
            job: None,
            comments: vec![ShownComment {
                responder: "andrew".to_string(),
                response: "    indented too\n".to_string(),
                created_at: String::new(),
            }],
        };

        let text = review_text(&review);

        assert!(text.contains("\n    fn main() {}\n"), "{text:?}");
        assert!(text.contains("Prose after.\n"), "{text:?}");
        assert!(
            text.contains("**andrew**\n\n    indented too\n"),
            "{text:?}"
        );
    }

    #[test]
    fn scrolling_dismisses_a_result_notice() {
        let review = (1..=20)
            .map(|line| format!("- line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut view = ReviewView::new(Some(42), Vec::new(), review, false);
        let mut terminal = Terminal::new(TestBackend::new(60, 8)).expect("test terminal");
        terminal
            .draw(|frame| view.render(frame))
            .expect("initial render");
        view.notice = Some("comment added".to_string());

        press(&mut view, KeyCode::Down);
        terminal
            .draw(|frame| view.render(frame))
            .expect("scrolled render");
        let frame: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        assert_eq!(view.scroll, 1);
        assert!(!frame.contains("comment added"), "{frame:?}");
        assert!(frame.contains("a close"), "{frame:?}");
    }

    #[test]
    fn a_notice_renders_on_its_own_line_above_the_key_hints() {
        let mut view = ReviewView::new(Some(42), Vec::new(), "review".to_string(), false);
        view.set_notice("review copied".to_string());
        let key_hints = view.footer();
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).expect("test terminal");
        terminal.draw(|frame| view.render(frame)).expect("render");

        let buffer = terminal.backend().buffer();
        let width = buffer.area.width as usize;
        let row = |n: usize| -> String {
            buffer.content()[n * width..(n + 1) * width]
                .iter()
                .map(|cell| cell.symbol())
                .collect()
        };
        let last_row = row(buffer.area.height as usize - 1);
        let notice_row = row(buffer.area.height as usize - 2);

        assert!(
            notice_row.trim_end().starts_with("review copied"),
            "{notice_row:?}"
        );
        assert!(!last_row.contains("review copied"), "{last_row:?}");
        assert!(last_row.trim_end() == key_hints, "{last_row:?}");
    }

    #[test]
    fn markdown_is_rendered_rather_than_printed_verbatim() {
        let mut view = ReviewView::new(
            Some(42),
            Vec::new(),
            "## Findings\n\n**High** severity in `src/exec.rs`\n".to_string(),
            false,
        );
        let mut terminal = Terminal::new(TestBackend::new(60, 8)).expect("test terminal");
        terminal.draw(|frame| view.render(frame)).expect("render");
        let buffer = terminal.backend().buffer();
        let frame: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        let severity = buffer
            .content()
            .iter()
            .find(|cell| cell.symbol() == "H")
            .expect("severity marker");

        // Markdown markers become styles, except heading markers.
        assert!(frame.contains("## Findings"), "{frame:?}");
        assert!(!frame.contains("**High**"), "{frame:?}");
        assert!(!frame.contains("`src/exec.rs`"), "{frame:?}");
        assert!(severity.modifier.contains(Modifier::BOLD), "{severity:?}");
    }
}
