//! Markdown description and title formatting for GitHub Pull Requests.

use nebula_proto::TraceId;

/// Formats a concise PR title from the task goal.
///
/// Truncates to a maximum length (e.g. 72 characters) and strips newlines.
#[must_use]
pub fn format_pr_title(goal: &str) -> String {
    let single_line = goal.lines().next().unwrap_or("").trim();
    if single_line.is_empty() {
        return "chore(task): automated task completed by Nebula".to_owned();
    }

    if single_line.len() > 72 {
        let mut truncated = single_line[..72].trim_end().to_owned();
        truncated.push_str("...");
        truncated
    } else {
        single_line.to_owned()
    }
}

/// Formats the markdown PR body with summary, goal, trace ID, verification results, and co-author trailers.
#[must_use]
pub fn format_pr_body(
    goal: &str,
    summary: &str,
    trace_id: TraceId,
    test_results: Option<&str>,
    bot_username: &str,
    extra_coauthor: Option<&str>,
) -> String {
    let clean_goal = goal.trim();
    let clean_summary = summary.trim();
    let clean_tests = test_results
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("- All steps completed and verified successfully.");

    let mut body = format!(
        "## Summary\n\
         {clean_summary}\n\n\
         ## Goal\n\
         {clean_goal}\n\n\
         ## Trace ID\n\
         `{trace_id}`\n\n\
         ## Verification & Test Results\n\
         {clean_tests}\n\n\
         ---\n\
         Co-authored-by: {bot_username} <{bot_username}@users.noreply.github.com>"
    );

    if let Some(author) = extra_coauthor.map(str::trim).filter(|s| !s.is_empty()) {
        body.push_str(&format!("\nCo-authored-by: {author}"));
    }

    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_pr_title() {
        assert_eq!(
            format_pr_title("fix failing test in test_calc.py"),
            "fix failing test in test_calc.py"
        );

        let long = "this is a very long goal description that exceeds seventy-two characters and should be truncated cleanly";
        let title = format_pr_title(long);
        assert!(title.len() <= 75);
        assert!(title.ends_with("..."));

        assert_eq!(
            format_pr_title(""),
            "chore(task): automated task completed by Nebula"
        );
    }

    #[test]
    fn test_format_pr_body() {
        let trace_id = TraceId::new();
        let body = format_pr_body(
            "Fix test_calc.py",
            "Fixed addition test assertion",
            trace_id,
            Some("- 1 passed in 0.05s"),
            "Nebula-dev-bot",
            Some("Human Owner <owner@example.com>"),
        );

        assert!(body.contains("## Summary\nFixed addition test assertion"));
        assert!(body.contains("## Goal\nFix test_calc.py"));
        assert!(body.contains(&format!("## Trace ID\n`{trace_id}`")));
        assert!(body.contains("## Verification & Test Results\n- 1 passed in 0.05s"));
        assert!(
            body.contains(
                "Co-authored-by: Nebula-dev-bot <Nebula-dev-bot@users.noreply.github.com>"
            )
        );
        assert!(body.contains("Co-authored-by: Human Owner <owner@example.com>"));
    }
}
