//! Integration tests: full parse → build_request pipeline.

use concordance::proposal::{build_request, parse_document};

const TWO_WP_DOC: &str = r#"---
title: Integration Test Proposal
api:
  voteId: "vote-int"
  status: draft
  estimatedDuration: "12"
  conversionRate: 0.25
  treasuryDonationTxHash: ""
---

## Executive Summary

Integration summary.

## Work Package 1: Alpha

### Budget

> wpTotal: 200000

- **Team** | Resources (Labor) | 200000: Alpha work.

---

## Work Package 2: Beta

### Budget

> wpTotal: 150000

- **Team** | Resources (Labor) | 150000: Beta work.

---

## Budget Summary

> totalBudget: 360500
"#;

#[test]
fn total_budget_survives_parse_and_build() {
    let doc = parse_document(TWO_WP_DOC).unwrap();
    assert_eq!(doc.total_budget, 360_500);
    let req = build_request(&doc);
    assert_eq!(req.meta_data["totalBudget"].as_u64().unwrap(), 360_500);
}

#[test]
fn work_package_count_matches_between_doc_and_request() {
    let doc = parse_document(TWO_WP_DOC).unwrap();
    assert_eq!(doc.work_packages.len(), 2);
    let req = build_request(&doc);
    let wps = req.meta_data["proposalDetails"]["workPackages"]
        .as_array()
        .expect("workPackages must be an array");
    assert_eq!(wps.len(), 2);
}

#[test]
fn work_package_names_and_budgets_round_trip() {
    let doc = parse_document(TWO_WP_DOC).unwrap();
    let req = build_request(&doc);
    let wps = req.meta_data["proposalDetails"]["workPackages"]
        .as_array()
        .unwrap();
    assert_eq!(wps[0]["name"], "Alpha");
    assert_eq!(wps[1]["name"], "Beta");
    assert_eq!(wps[0]["totalBudget"].as_u64().unwrap(), 200_000);
    assert_eq!(wps[1]["totalBudget"].as_u64().unwrap(), 150_000);
}
