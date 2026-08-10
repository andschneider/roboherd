use crate::roborev::ReviewJob;

/// The counts behind one workspace's sidebar tokens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Badge {
    /// Completed reviews with a failing verdict that nobody has closed.
    pub failing: usize,
    /// Completed reviews with a passing verdict that nobody has closed. Closing a review drops it
    /// from this count, which keeps it a queue rather than a running tally.
    pub passing: usize,
    /// Reviews queued or running.
    pub active: usize,
}

impl Badge {
    /// Aggregate a workspace's review jobs into badge counts.
    pub fn from_jobs(jobs: &[ReviewJob]) -> Self {
        Badge {
            failing: jobs.iter().filter(|job| job.needs_attention()).count(),
            passing: jobs.iter().filter(|job| job.is_open_pass()).count(),
            active: jobs.iter().filter(|job| job.status.is_active()).count(),
        }
    }

    /// The three token values herdr renders in the sidebar, each absent when its count is zero.
    ///
    /// Each count is reviews rather than findings, since `roborev list --json` exposes only a
    /// per-job verdict.
    pub fn tokens(self) -> [Option<String>; 3] {
        [
            token('×', self.failing),
            token('✓', self.passing),
            token('●', self.active),
        ]
    }
}

/// One token, absent at zero so the row carries only live counts.
fn token(glyph: char, count: usize) -> Option<String> {
    (count > 0).then(|| format!("{glyph}{count}"))
}

#[cfg(test)]
mod tests {
    use super::Badge;
    use crate::roborev::ReviewJob;

    fn jobs(json: &str) -> Vec<ReviewJob> {
        serde_json::from_str(json).expect("valid job array")
    }

    /// The set tokens as one string, joined the way herdr's separator renders them, or `None` when
    /// every token is cleared.
    fn rendered(badge: Badge) -> Option<String> {
        let set: Vec<String> = badge.tokens().into_iter().flatten().collect();
        (!set.is_empty()).then(|| set.join(" · "))
    }

    #[test]
    fn no_jobs_clears_the_tokens() {
        assert_eq!(rendered(Badge::from_jobs(&[])), None);
    }

    #[test]
    fn a_zero_count_leaves_no_token() {
        let badge = Badge::from_jobs(&jobs(r#"[{"id":346,"status":"running"}]"#));
        assert_eq!(rendered(badge).as_deref(), Some("●1"));
    }

    #[test]
    fn queued_and_running_reviews_both_count_as_running() {
        let badge = Badge::from_jobs(&jobs(
            r#"[{"id":346,"status":"running"},{"id":347,"status":"queued"}]"#,
        ));
        assert_eq!(rendered(badge).as_deref(), Some("●2"));
    }

    /// Job 2 pairs an errored status with a verdict, which only a Postgres sync produces. A verdict
    /// counts wherever it appears.
    #[test]
    fn open_reviews_split_by_verdict() {
        let badge = Badge::from_jobs(&jobs(
            r#"[{"id":1,"status":"done","closed":false,"verdict":"F"},
                {"id":2,"status":"failed","closed":false,"verdict":"F"},
                {"id":3,"status":"done","closed":false,"verdict":"P"},
                {"id":4,"status":"running"}]"#,
        ));
        assert_eq!(rendered(badge).as_deref(), Some("×2 · ✓1 · ●1"));
    }

    #[test]
    fn a_closed_review_leaves_every_count() {
        let badge = Badge::from_jobs(&jobs(
            r#"[{"id":9,"status":"queued"},{"id":99,"status":"done","closed":true,"verdict":"P"}]"#,
        ));
        assert_eq!(rendered(badge).as_deref(), Some("●1"));
    }

    #[test]
    fn a_review_with_no_verdict_counts_as_neither() {
        let badge = Badge::from_jobs(&jobs(r#"[{"id":1,"status":"done","closed":false}]"#));
        assert_eq!(rendered(badge), None);
    }

    /// An errored review without a verdict reaches no count. The reporter toast covers it.
    #[test]
    fn an_errored_review_reaches_no_count() {
        let badge = Badge::from_jobs(&jobs(r#"[{"id":1,"status":"failed","closed":false}]"#));
        assert_eq!(rendered(badge), None);
    }

    #[test]
    fn settled_reviews_produce_no_tokens() {
        let badge = Badge::from_jobs(&jobs(
            r#"[{"id":1,"status":"done","closed":true,"verdict":"F"},
                {"id":2,"status":"done","closed":true,"verdict":"P"},
                {"id":3,"status":"canceled"},
                {"id":4,"status":"skipped"}]"#,
        ));
        assert_eq!(rendered(badge), None);
    }
}
