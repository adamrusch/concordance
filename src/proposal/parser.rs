//! Markdown + YAML frontmatter parser for proposal documents.
//!
//! The expected document format is:
//! ```text
//! ---
//! title: ...
//! api:
//!   voteId: ...
//! ---
//!
//! ## Executive Summary
//! ...
//! ## Work Package 1: Name
//! ...
//! ## Budget Summary
//! > totalBudget: N
//! ```
//!
//! Sections are located by heading regex and extracted as raw text. Work
//! packages and their sub-sections (milestones, budget) are parsed
//! independently.

use regex::Regex;
use std::sync::OnceLock;

use crate::error::{Error, Result};

use super::types::{
    BudgetItem, Frontmatter, Milestone, ProposalDocument, SupportingDoc, WorkPackage,
};

// ── Frontmatter ───────────────────────────────────────────────────────────────

pub fn parse_document(content: &str) -> Result<ProposalDocument> {
    let (fm, body) = split_frontmatter(content)?;
    let frontmatter: Frontmatter = serde_yaml::from_str(&fm)?;

    let summary = extract_section(&body, r"(?m)^## Executive Summary\s*$", Some(r"(?m)^## \w"));
    let track_record = extract_section(
        &body,
        r"(?m)^## Track Record\s*$",
        Some(r"(?m)^## Duration"),
    );
    let pillar_rationale = extract_section(
        &body,
        r"(?m)^### Pillar Rationale\s*$",
        Some(r"(?m)^### KPI Alignment"),
    );
    let kpi_alignment =
        extract_section(&body, r"(?m)^### KPI Alignment\s*$", Some(r"(?m)^---\s*$"));

    let work_packages = parse_work_packages(&body)?;

    let budget_summary_raw =
        extract_section(&body, r"(?m)^## Budget Summary\s*$", Some(r"(?m)^## "));
    let total_budget = parse_total_budget_line(&budget_summary_raw)
        .or_else(|| frontmatter.api.proposal_id.as_ref().map(|_| 0))
        .unwrap_or(0);

    Ok(ProposalDocument {
        frontmatter,
        summary,
        track_record,
        pillar_rationale,
        kpi_alignment,
        work_packages,
        total_budget,
    })
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn split_frontmatter(content: &str) -> Result<(String, String)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?s)^---\n(.*?)\n---\n").unwrap());

    let caps = re
        .captures(content)
        .ok_or_else(|| Error::Parse("no YAML frontmatter found".into()))?;
    let fm = caps[1].to_string();
    let body = content[caps[0].len()..].to_string();
    Ok((fm, body))
}

/// Extract text from `start_re` to `end_re` (or end of string).
pub fn extract_section(text: &str, start_re: &str, end_re: Option<&str>) -> String {
    let start = match Regex::new(start_re).ok().and_then(|r| r.find(text)) {
        Some(m) => m.end(),
        None => return String::new(),
    };
    let slice = &text[start..];
    let end = end_re
        .and_then(|p| Regex::new(p).ok())
        .and_then(|r| r.find(slice))
        .map(|m| m.start())
        .unwrap_or(slice.len());
    slice[..end].trim().to_string()
}

fn parse_total_budget_line(text: &str) -> Option<u64> {
    Regex::new(r">\s*totalBudget:\s*(\d+)")
        .ok()?
        .captures(text)?[1]
        .parse()
        .ok()
}

// ── Work packages ─────────────────────────────────────────────────────────────

fn parse_work_packages(body: &str) -> Result<Vec<WorkPackage>> {
    static WP_HDR: OnceLock<Regex> = OnceLock::new();
    let hdr_re = WP_HDR.get_or_init(|| Regex::new(r"(?m)^## Work Package \d+:\s*(.+?)$").unwrap());

    let headers: Vec<_> = hdr_re.find_iter(body).collect();
    let body_end = Regex::new(r"(?m)^## (Budget Summary|Team)\b")
        .unwrap()
        .find(body)
        .map(|m| m.start())
        .unwrap_or(body.len());

    let mut wps = Vec::new();
    for (i, hm) in headers.iter().enumerate() {
        let name = hdr_re
            .captures(hm.as_str())
            .map(|c| c[1].trim().to_string())
            .unwrap_or_default();
        let start = hm.end();
        let end = headers.get(i + 1).map(|h| h.start()).unwrap_or(body_end);
        wps.push(parse_wp(&body[start..end], name)?);
    }
    Ok(wps)
}

fn parse_wp(wp_body: &str, name: String) -> Result<WorkPackage> {
    let summary = extract_section(wp_body, r"(?m)^### Summary\s*$", Some(r"(?m)^### "));
    let obj_raw = extract_section(wp_body, r"(?m)^### Core Objectives\s*$", Some(r"(?m)^### "));
    let expected_value =
        extract_section(wp_body, r"(?m)^### Expected Value\s*$", Some(r"(?m)^### "));
    let metric = extract_section(wp_body, r"(?m)^### Success Metrics\s*$", Some(r"(?m)^### "));
    let milestones_raw = extract_section(wp_body, r"(?m)^### Milestones\s*$", Some(r"(?m)^### "));
    let docs_raw = extract_section(
        wp_body,
        r"(?m)^### Supporting Documents\s*$",
        Some(r"(?m)^### "),
    );
    let budget_raw = extract_section(wp_body, r"(?m)^### Budget\s*$", Some(r"(?m)^---\s*$"));

    let core_objectives = parse_objectives(&obj_raw);
    let milestones = parse_milestones(&milestones_raw);
    let supporting_documents = parse_supporting_docs(&docs_raw);
    let budget_breakdown = parse_budget(&budget_raw);

    let wp_total = Regex::new(r">\s*wpTotal:\s*(\d+)")
        .unwrap()
        .captures(&budget_raw)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(0u64);

    let (initiative_type, proposal_type) = parse_wp_meta(wp_body);

    Ok(WorkPackage {
        name,
        summary,
        initiative_type,
        proposal_type,
        supporting_documents,
        core_objectives,
        expected_value,
        metric,
        milestones,
        budget_breakdown,
        total_budget: wp_total,
    })
}

fn parse_wp_meta(wp_body: &str) -> (String, String) {
    static INIT: OnceLock<Regex> = OnceLock::new();
    static PROP: OnceLock<Regex> = OnceLock::new();
    let init_re = INIT.get_or_init(|| Regex::new(r">\s*initiativeType:\s*(.+)").unwrap());
    let prop_re = PROP.get_or_init(|| Regex::new(r">\s*proposalType:\s*(.+)").unwrap());

    let mut initiative_type = "Maintenance".to_string();
    let mut proposal_type = "Technical (software/IT)".to_string();
    for line in wp_body.lines().take(10) {
        if let Some(caps) = init_re.captures(line) {
            initiative_type = caps[1].trim().to_string();
        }
        if let Some(caps) = prop_re.captures(line) {
            proposal_type = caps[1].trim().to_string();
        }
    }
    (initiative_type, proposal_type)
}

fn parse_objectives(raw: &str) -> Vec<String> {
    static SPLIT: OnceLock<Regex> = OnceLock::new();
    static ITEM: OnceLock<Regex> = OnceLock::new();
    let split_re = SPLIT.get_or_init(|| Regex::new(r"(?m)^\d+\.\s+").unwrap());
    let item_re = ITEM.get_or_init(|| Regex::new(r"(?s)\*\*(.+?)\*\*:\s*(.+)").unwrap());
    split_re
        .split(raw)
        .skip(1)
        .filter_map(|chunk| {
            let caps = item_re.captures(chunk)?;
            let desc = caps[2].split_whitespace().collect::<Vec<_>>().join(" ");
            Some(format!("**{}**: {}", caps[1].trim(), desc))
        })
        .collect()
}

fn parse_milestones(raw: &str) -> Vec<Milestone> {
    static SPLIT: OnceLock<Regex> = OnceLock::new();
    let split_re = SPLIT.get_or_init(|| Regex::new(r"(?m)^#### Milestone \d+:").unwrap());

    split_re
        .split(raw)
        .skip(1) // skip text before first milestone
        .filter_map(|block| {
            let name_re = Regex::new(r"(?s)\s*(.*?)\s*\(Duration:\s*(\d+)\s*weeks?\)").unwrap();
            let nm = name_re.captures(block)?;
            // No lookahead needed: consume up to the literal boundary marker
            let deliverables =
                Regex::new(r"(?s)\*\*Deliverables:\*\*\s*(.*?)\*\*Acceptance Criteria:\*\*")
                    .unwrap()
                    .captures(block)
                    .map(|c| c[1].trim().to_string())
                    .unwrap_or_default();
            // Acceptance criteria runs to end of block (block is already split by #### Milestone)
            let acceptance_criteria = Regex::new(r"(?s)\*\*Acceptance Criteria:\*\*\s*(.+)")
                .unwrap()
                .captures(block)
                .map(|c| c[1].trim().to_string())
                .unwrap_or_default();
            Some(Milestone {
                name: nm[1].trim().to_string(),
                deliverables,
                acceptance_criteria,
                duration: nm[2].to_string(),
            })
        })
        .collect()
}

fn parse_supporting_docs(raw: &str) -> Vec<SupportingDoc> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").unwrap());
    re.captures_iter(raw)
        .map(|c| SupportingDoc {
            title: c[1].to_string(),
            url: c[2].to_string(),
        })
        .collect()
}

fn parse_budget(raw: &str) -> Vec<BudgetItem> {
    static ITEM_RE: OnceLock<Regex> = OnceLock::new();
    let item_re = ITEM_RE.get_or_init(|| {
        Regex::new(r"- \*\*([^*]+)\*\*\s*\|\s*([^|]+)\s*\|\s*(\d+)(?:\s*\|[^:]+)?:\s*(.*)").unwrap()
    });

    let mut items: Vec<BudgetItem> = Vec::new();
    let mut current: Option<BudgetItem> = None;

    for line in raw.lines() {
        if line.starts_with("- **") {
            if let Some(item) = current.take() {
                items.push(item);
            }
            if line.contains("| CONDITIONAL") {
                continue;
            }
            if let Some(caps) = item_re.captures(line) {
                let amount: u64 = caps[3].parse().unwrap_or(0);
                current = Some(BudgetItem {
                    name: caps[1].trim().to_string(),
                    cost_category: caps[2].trim().to_string(),
                    description: caps[4].trim().to_string(),
                    quantity: 1,
                    unit_price: amount,
                    total: amount,
                });
            }
        } else if let Some(ref mut item) = current {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('>') {
                item.description.push(' ');
                item.description.push_str(trimmed);
            }
        }
    }
    if let Some(item) = current {
        items.push(item);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal but complete proposal fixture used across multiple tests.
    const MINIMAL_PROPOSAL: &str = r#"---
title: Test Proposal
api:
  voteId: "vote123"
  status: draft
  estimatedDuration: "12"
  conversionRate: "0.25"
  treasuryDonationTxHash: "txhash"
---

# Test Proposal

## Executive Summary

This is the summary text.

## Track Record

This is the track record.

## Duration

6 months

## Strategy Alignment

### Pillar Rationale

Pillar rationale goes here.

### KPI Alignment

KPI alignment goes here.

---

## Work Package 1: Core Work

> initiativeType: Maintenance
> proposalType: Technical (software/IT)

### Summary

WP summary here.

### Core Objectives

1. **First Goal**: Do the first thing well.

2. **Second Goal**: Do the second thing well.

### Expected Value

Expected value text.

### Success Metrics

- Ship on time

### Milestones

#### Milestone 1: Initial Delivery (Duration: 8 weeks)

**Deliverables:**
- The thing is delivered

**Acceptance Criteria:**
- The thing works

### Supporting Documents

- [GitHub](https://github.com/example/repo)

### Budget

> wpTotal: 100000

- **Team** | Resources (Labor) | 100000: Full team effort.

---

## Budget Summary

> totalBudget: 103000

| Work Package | ADA | USD |
|---|---|---|
| WP1: Core Work | 100,000 | $25,000 |
| **Subtotal** | **100,000** | **$25,000** |
"#;

    #[test]
    fn parse_minimal_proposal_succeeds() {
        let doc = parse_document(MINIMAL_PROPOSAL).unwrap();
        assert_eq!(doc.frontmatter.title, "Test Proposal");
        assert_eq!(doc.frontmatter.api.vote_id, "vote123");
        assert!(doc.summary.contains("summary text"));
        assert!(doc.track_record.contains("track record"));
    }

    #[test]
    fn parse_work_packages() {
        let doc = parse_document(MINIMAL_PROPOSAL).unwrap();
        assert_eq!(doc.work_packages.len(), 1);
        let wp = &doc.work_packages[0];
        assert_eq!(wp.name, "Core Work");
        assert_eq!(wp.initiative_type, "Maintenance");
    }

    #[test]
    fn parse_objectives() {
        let doc = parse_document(MINIMAL_PROPOSAL).unwrap();
        let objs = &doc.work_packages[0].core_objectives;
        assert_eq!(objs.len(), 2);
        assert!(objs[0].contains("First Goal"));
        assert!(objs[1].contains("Second Goal"));
    }

    #[test]
    fn parse_milestones() {
        let doc = parse_document(MINIMAL_PROPOSAL).unwrap();
        let ms = &doc.work_packages[0].milestones;
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].name, "Initial Delivery");
        assert_eq!(ms[0].duration, "8");
        assert!(ms[0].deliverables.contains("thing is delivered"));
        assert!(ms[0].acceptance_criteria.contains("thing works"));
    }

    #[test]
    fn parse_budget_items() {
        let doc = parse_document(MINIMAL_PROPOSAL).unwrap();
        let items = &doc.work_packages[0].budget_breakdown;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Team");
        assert_eq!(items[0].total, 100_000);
    }

    #[test]
    fn parse_total_budget_from_body() {
        let doc = parse_document(MINIMAL_PROPOSAL).unwrap();
        assert_eq!(doc.total_budget, 103_000);
    }

    #[test]
    fn conditional_budget_items_excluded() {
        let raw = r#"---
title: T
api:
  voteId: v
---
## Work Package 1: WP

### Budget

> wpTotal: 50000

- **Real Item** | Labor | 50000: Does stuff.
- **Conditional Item** | CONDITIONAL | 20000: Not included.

## Budget Summary

> totalBudget: 51500
"#;
        let doc = parse_document(raw).unwrap();
        let items = &doc.work_packages[0].budget_breakdown;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Real Item");
    }

    #[test]
    fn missing_frontmatter_errors() {
        assert!(parse_document("no frontmatter here").is_err());
    }

    #[test]
    fn extract_section_returns_empty_when_missing() {
        let text = "## Other Section\n\nContent.";
        let result = extract_section(text, r"(?m)^## Missing\s*$", Some(r"(?m)^## "));
        assert_eq!(result, "");
    }

    #[test]
    fn extract_section_stops_at_end_pattern() {
        let text = "## Start\n\nkeep this\n\n## End\n\nignore this";
        let result = extract_section(text, r"(?m)^## Start\s*$", Some(r"(?m)^## End"));
        assert!(result.contains("keep this"));
        assert!(!result.contains("ignore this"));
    }

    // Multi-WP fixture: two work packages with distinct names and budgets.
    const TWO_WP_PROPOSAL: &str = r#"---
title: Multi-WP Proposal
api:
  voteId: "vote123"
---

## Work Package 1: Alpha Work

> initiativeType: Development
> proposalType: Technical

### Summary

Alpha WP summary.

### Milestones

#### Milestone 1: Alpha Delivery (Duration: 4 weeks)

**Deliverables:**
- Alpha delivered

**Acceptance Criteria:**
- Alpha works

### Budget

> wpTotal: 100000

- **Alpha Team** | Resources (Labor) | 100000: Alpha labor.

---

## Work Package 2: Beta Work

> initiativeType: Maintenance
> proposalType: Technical

### Summary

Beta WP summary.

### Milestones

#### Milestone 1: Beta Delivery (Duration: 6 weeks)

**Deliverables:**
- Beta delivered

**Acceptance Criteria:**
- Beta works

### Budget

> wpTotal: 50000

- **Beta Team** | Resources (Labor) | 50000: Beta labor.

---

## Budget Summary

> totalBudget: 154500
"#;

    #[test]
    fn multi_wp_count() {
        let doc = parse_document(TWO_WP_PROPOSAL).unwrap();
        assert_eq!(doc.work_packages.len(), 2);
    }

    #[test]
    fn multi_wp_names() {
        let doc = parse_document(TWO_WP_PROPOSAL).unwrap();
        assert_eq!(doc.work_packages[0].name, "Alpha Work");
        assert_eq!(doc.work_packages[1].name, "Beta Work");
    }

    #[test]
    fn multi_wp_individual_and_total_budgets() {
        let doc = parse_document(TWO_WP_PROPOSAL).unwrap();
        assert_eq!(doc.work_packages[0].total_budget, 100_000);
        assert_eq!(doc.work_packages[1].total_budget, 50_000);
        assert_eq!(doc.total_budget, 154_500);
    }

    #[test]
    fn multi_wp_initiative_types_parsed_independently() {
        let doc = parse_document(TWO_WP_PROPOSAL).unwrap();
        assert_eq!(doc.work_packages[0].initiative_type, "Development");
        assert_eq!(doc.work_packages[1].initiative_type, "Maintenance");
    }
}
