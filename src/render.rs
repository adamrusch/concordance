//! Markdown rendering for API proposal objects.
//!
//! Converts the raw JSON returned by the Ekklesia API into a human-readable
//! markdown document. The output mirrors the structure of the source markdown
//! used when submitting proposals, so fetched proposals can be diffed against
//! local files.

use serde_json::Value;

use crate::api::Proposal;

pub fn render_proposal_md(p: &Proposal) -> String {
    let mut out = String::new();
    let meta = p.meta_data.as_ref();

    out.push_str(&format!("# {}\n\n", p.title));

    out.push_str(&format!("**Status:** {}  \n", p.status));
    if let Some(at) = p.submitted_at {
        out.push_str(&format!("**Submitted:** {}  \n", at.format("%Y-%m-%d")));
    }
    if let Some(cc) = p.comment_count {
        out.push_str(&format!("**Comments:** {}  \n", cc));
    }

    if let Some(budget) = meta
        .and_then(|m| m.get("totalBudget"))
        .and_then(|v| v.as_u64())
    {
        let rate = meta
            .and_then(|m| m.get("proposalDetails"))
            .and_then(|pd| pd.get("conversionRate"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.25);
        let usd = (budget as f64) * rate;
        out.push_str(&format!(
            "**Budget:** {} ADA (~${:.0} USD at ${}/ADA)  \n",
            fmt_ada(budget),
            usd,
            rate
        ));
    }
    out.push('\n');

    // Executive Summary
    if let Some(s) = p.summary.as_deref().filter(|s| !s.is_empty()) {
        out.push_str("## Executive Summary\n\n");
        out.push_str(s);
        out.push_str("\n\n");
    }

    // Track Record
    if let Some(tr) = meta
        .and_then(|m| m.get("trackRecord"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        out.push_str("## Track Record\n\n");
        out.push_str(tr);
        out.push_str("\n\n");
    }

    // Prior Funding
    if let Some(pf) = meta
        .and_then(|m| m.get("priorFunding"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        out.push_str("## Prior Funding\n\n");
        out.push_str(pf);
        out.push_str("\n\n");
    }

    // Strategy Framework
    if let Some(sf) = meta.and_then(|m| m.get("strategyFramework")) {
        if let Some(pr) = sf
            .get("pillarRationale")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            out.push_str("## Strategy Alignment\n\n");
            out.push_str(pr);
            out.push_str("\n\n");
        }
        if let Some(kpi) = sf
            .get("kpiAlignment")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            out.push_str("### KPI Alignment\n\n");
            out.push_str(kpi);
            out.push_str("\n\n");
        }
    }

    // Work Packages
    if let Some(wps) = meta
        .and_then(|m| m.get("proposalDetails"))
        .and_then(|pd| pd.get("workPackages"))
        .and_then(|v| v.as_array())
    {
        for (i, wp) in wps.iter().enumerate() {
            render_work_package(&mut out, i + 1, wp);
        }
    }

    // Budget Summary
    render_budget_summary(&mut out, meta);

    // Treasury / Admin
    if let Some(rep) = meta
        .and_then(|m| m.get("treasuryRepaymentCircumstances"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        out.push_str("## Treasury Repayment\n\n");
        out.push_str(rep);
        out.push_str("\n\n");
    }

    out
}

fn render_work_package(out: &mut String, n: usize, wp: &Value) {
    let name = wp
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Work Package");
    out.push_str(&format!("## Work Package {n}: {name}\n\n"));

    if let Some(it) = wp.get("initiativeType").and_then(|v| v.as_str()) {
        out.push_str(&format!("> initiativeType: {it}\n"));
    }
    if let Some(pt) = wp.get("proposalType").and_then(|v| v.as_str()) {
        out.push_str(&format!("> proposalType: {pt}\n"));
    }
    out.push('\n');

    if let Some(s) = wp
        .get("summary")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        out.push_str("### Summary\n\n");
        out.push_str(s);
        out.push_str("\n\n");
    }

    if let Some(objs) = wp.get("coreObjectives").and_then(|v| v.as_array()) {
        if !objs.is_empty() {
            out.push_str("### Core Objectives\n\n");
            for (i, obj) in objs.iter().enumerate() {
                if let Some(s) = obj.as_str() {
                    out.push_str(&format!("{}. {}\n\n", i + 1, s));
                }
            }
        }
    }

    if let Some(ev) = wp
        .get("expectedValue")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        out.push_str("### Expected Value\n\n");
        out.push_str(ev);
        out.push_str("\n\n");
    }

    if let Some(m) = wp
        .get("metric")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        out.push_str("### Success Metrics\n\n");
        out.push_str(m);
        out.push_str("\n\n");
    }

    if let Some(milestones) = wp.get("milestones").and_then(|v| v.as_array()) {
        if !milestones.is_empty() {
            out.push_str("### Milestones\n\n");
            for ms in milestones {
                let ms_name = ms
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Milestone");
                let dur = ms.get("duration").and_then(|v| v.as_u64()).unwrap_or(0);
                out.push_str(&format!("#### {ms_name} (Duration: {dur} weeks)\n\n"));
                if let Some(d) = ms
                    .get("deliverables")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    out.push_str("**Deliverables:**\n");
                    out.push_str(d);
                    out.push_str("\n\n");
                }
                if let Some(ac) = ms
                    .get("acceptanceCriteria")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    out.push_str("**Acceptance Criteria:**\n");
                    out.push_str(ac);
                    out.push_str("\n\n");
                }
            }
        }
    }

    if let Some(budget) = wp.get("budgetBreakdown").and_then(|v| v.as_array()) {
        if !budget.is_empty() {
            out.push_str("### Budget\n\n");
            let mut wp_total: u64 = 0;
            for item in budget {
                let iname = item.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let cat = item
                    .get("costCategory")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let amt = item.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
                let desc = item
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                wp_total += amt;
                out.push_str(&format!(
                    "- **{iname}** | {cat} | {} ADA: {desc}\n",
                    fmt_ada(amt)
                ));
            }
            out.push_str(&format!("\n**WP Total:** {} ADA\n\n", fmt_ada(wp_total)));
        }
    }

    if let Some(docs) = wp.get("supportingDocuments").and_then(|v| v.as_array()) {
        if !docs.is_empty() {
            out.push_str("### Supporting Documents\n\n");
            for doc in docs {
                let title = doc.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                let url = doc.get("url").and_then(|v| v.as_str()).unwrap_or("#");
                out.push_str(&format!("- [{title}]({url})\n"));
            }
            out.push('\n');
        }
    }

    out.push_str("---\n\n");
}

fn render_budget_summary(out: &mut String, meta: Option<&Value>) {
    let wps = match meta
        .and_then(|m| m.get("proposalDetails"))
        .and_then(|pd| pd.get("workPackages"))
        .and_then(|v| v.as_array())
    {
        Some(w) if !w.is_empty() => w,
        _ => return,
    };

    let rate = meta
        .and_then(|m| m.get("proposalDetails"))
        .and_then(|pd| pd.get("conversionRate"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.25);

    out.push_str("## Budget Summary\n\n");
    out.push_str(&format!("| Work Package | ADA | USD (@ ${rate}/ADA) |\n"));
    out.push_str("|---|---|---|\n");

    let mut grand_total: u64 = 0;
    for (i, wp) in wps.iter().enumerate() {
        let name = wp
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Work Package");
        let wp_total: u64 = wp
            .get("budgetBreakdown")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|it| it.get("total").and_then(|v| v.as_u64()))
                    .sum()
            })
            .unwrap_or(0);
        grand_total += wp_total;
        let usd = (wp_total as f64) * rate;
        out.push_str(&format!(
            "| WP{}: {} | {} | ${:.0} |\n",
            i + 1,
            name,
            fmt_ada(wp_total),
            usd
        ));
    }

    let grand_usd = (grand_total as f64) * rate;
    out.push_str(&format!(
        "| **Total** | **{}** | **${:.0}** |\n\n",
        fmt_ada(grand_total),
        grand_usd
    ));
}

/// Convert a proposal title to a filesystem-safe slug.
///
/// Rules: lowercase, non-alphanumeric characters become hyphens, consecutive
/// hyphens are collapsed, leading/trailing hyphens are stripped, maximum
/// length is 64 characters (trimmed at a word boundary).
pub fn title_to_slug(title: &str) -> String {
    let slug: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.len() > 64 {
        // trim at word boundary
        let trimmed = &slug[..64];
        trimmed.trim_end_matches('-').to_string()
    } else {
        slug
    }
}

fn fmt_ada(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── title_to_slug ─────────────────────────────────────────────────────────

    #[test]
    fn slug_basic() {
        assert_eq!(title_to_slug("Hello World"), "hello-world");
    }

    #[test]
    fn slug_collapses_runs_of_specials() {
        assert_eq!(
            title_to_slug("Hello: World & Things!"),
            "hello-world-things"
        );
    }

    #[test]
    fn slug_strips_leading_trailing_hyphens() {
        let s = title_to_slug("!!! Leading and trailing !!!");
        assert!(!s.starts_with('-'));
        assert!(!s.ends_with('-'));
    }

    #[test]
    fn slug_only_lowercase_alphanumeric_and_hyphens() {
        let s = title_to_slug("Rust: Fast & Reliable — A Story (2026)");
        assert!(
            s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        );
    }

    #[test]
    fn slug_max_64_chars() {
        let long = "A".repeat(200);
        let s = title_to_slug(&long);
        assert!(s.len() <= 64);
    }

    #[test]
    fn slug_truncation_does_not_end_with_hyphen() {
        // 65 'a' chars separated by spaces → slug is "a-a-a-..." potentially cut mid-word
        let title: String = std::iter::repeat_n("word", 30)
            .collect::<Vec<_>>()
            .join(" ");
        let s = title_to_slug(&title);
        assert!(s.len() <= 64);
        assert!(!s.ends_with('-'));
    }

    #[test]
    fn slug_empty_input() {
        assert_eq!(title_to_slug(""), "");
        assert_eq!(title_to_slug("!!!"), "");
    }

    #[test]
    fn slug_unicode_becomes_hyphens() {
        let s = title_to_slug("Ouroboros Tachýs");
        // ý is non-ASCII alphanumeric; treated as separator
        assert!(
            s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        );
    }

    // ── fmt_ada ───────────────────────────────────────────────────────────────

    #[test]
    fn fmt_ada_small() {
        assert_eq!(fmt_ada(0), "0");
        assert_eq!(fmt_ada(999), "999");
        assert_eq!(fmt_ada(1000), "1,000");
    }

    #[test]
    fn fmt_ada_millions() {
        assert_eq!(fmt_ada(1_000_000), "1,000,000");
        assert_eq!(fmt_ada(1_153_600), "1,153,600");
    }

    // ── render_proposal_md ────────────────────────────────────────────────────

    fn bare_proposal() -> Proposal {
        Proposal {
            id: "test-id".into(),
            vote_id: None,
            title: "My Test Proposal".into(),
            summary: Some("The summary text.".into()),
            status: "live".into(),
            proposer_id: None,
            submitted_at: None,
            version: None,
            comment_count: Some(5),
            meta_data: None,
        }
    }

    fn proposal_with_meta() -> Proposal {
        use serde_json::json;
        Proposal {
            meta_data: Some(json!({
                "totalBudget": 1_030_000u64,
                "trackRecord": "We shipped things.",
                "treasuryRepaymentCircumstances": "Unused funds returned.",
                "proposalDetails": {
                    "conversionRate": 0.25,
                    "workPackages": [{
                        "name": "Core Work",
                        "summary": "WP summary here.",
                        "initiativeType": "Maintenance",
                        "proposalType": "Technical",
                        "coreObjectives": ["**Goal A**: Do it."],
                        "expectedValue": "High value.",
                        "metric": "Ships on time.",
                        "milestones": [{
                            "name": "M1",
                            "duration": 8,
                            "deliverables": "Deliver the thing.",
                            "acceptanceCriteria": "Thing is delivered."
                        }],
                        "budgetBreakdown": [
                            {"name": "Team", "costCategory": "Labor", "total": 800_000u64, "description": "Dev work."},
                            {"name": "Infra", "costCategory": "Infrastructure", "total": 200_000u64, "description": "CI costs."}
                        ],
                        "supportingDocuments": [
                            {"title": "GitHub", "url": "https://github.com/example/repo"}
                        ]
                    }]
                }
            })),
            ..bare_proposal()
        }
    }

    #[test]
    fn render_title_is_h1() {
        let md = render_proposal_md(&bare_proposal());
        assert!(md.starts_with("# My Test Proposal\n"));
    }

    #[test]
    fn render_without_meta_does_not_panic() {
        let md = render_proposal_md(&bare_proposal());
        assert!(md.contains("# My Test Proposal"));
        assert!(md.contains("**Status:** live"));
        assert!(md.contains("**Comments:** 5"));
    }

    #[test]
    fn render_without_meta_no_budget_line() {
        // meta_data is None so no totalBudget is available
        let md = render_proposal_md(&bare_proposal());
        assert!(!md.contains("**Budget:**"));
    }

    #[test]
    fn render_summary_section_present() {
        let md = render_proposal_md(&bare_proposal());
        assert!(md.contains("## Executive Summary"));
        assert!(md.contains("The summary text."));
    }

    #[test]
    fn render_with_meta_includes_work_package_heading() {
        let md = render_proposal_md(&proposal_with_meta());
        assert!(md.contains("## Work Package 1: Core Work"));
    }

    #[test]
    fn render_with_meta_includes_track_record() {
        let md = render_proposal_md(&proposal_with_meta());
        assert!(md.contains("## Track Record"));
        assert!(md.contains("We shipped things."));
    }

    #[test]
    fn render_budget_summary_table_present() {
        let md = render_proposal_md(&proposal_with_meta());
        assert!(md.contains("## Budget Summary"));
        assert!(md.contains("1,000,000")); // sum of budget breakdown items
        assert!(md.contains("Core Work"));
    }

    #[test]
    fn render_budget_summary_totals_are_correct() {
        let md = render_proposal_md(&proposal_with_meta());
        // WP breakdown: 800,000 + 200,000 = 1,000,000
        assert!(md.contains("| WP1: Core Work | 1,000,000 |"));
        assert!(md.contains("| **Total** | **1,000,000**"));
    }

    #[test]
    fn render_treasury_repayment_conditional_on_content() {
        let with_repayment = render_proposal_md(&proposal_with_meta());
        assert!(with_repayment.contains("## Treasury Repayment"));
        assert!(with_repayment.contains("Unused funds returned."));

        // Without it in meta → section absent
        let without = render_proposal_md(&bare_proposal());
        assert!(!without.contains("## Treasury Repayment"));
    }

    #[test]
    fn render_budget_header_includes_rate() {
        let md = render_proposal_md(&proposal_with_meta());
        assert!(md.contains("$0.25/ADA"));
    }

    #[test]
    fn render_milestone_appears_under_wp() {
        let md = render_proposal_md(&proposal_with_meta());
        assert!(md.contains("#### M1 (Duration: 8 weeks)"));
        assert!(md.contains("Deliver the thing."));
        assert!(md.contains("Thing is delivered."));
    }

    #[test]
    fn render_supporting_documents_appear() {
        let md = render_proposal_md(&proposal_with_meta());
        assert!(md.contains("[GitHub](https://github.com/example/repo)"));
    }
}
