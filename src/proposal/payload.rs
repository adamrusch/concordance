//! Converts a parsed [`ProposalDocument`] into the JSON payload expected by
//! the Ekklesia API (`POST /api/v0/proposals`, `PUT /api/v0/proposals/:id`).
//!
//! The shape mirrors the Python `submit_proposal.py` script this tool replaces.
//! Key invariants:
//! - `metaData.proposalDetails.workPackages` holds the work package array
//! - `estimatedDuration` and `conversionRate` are serialized as strings in the top-level metadata
//! - Budget lines marked `| CONDITIONAL` in the source markdown are excluded from `budgetBreakdown`

use serde_json::{Value, json};

use crate::api::CreateProposalRequest;

use super::types::{BudgetItem, Milestone, ProposalDocument, SupportingDoc, WorkPackage};

/// Build the API request body from a parsed proposal document.
pub fn build_request(doc: &ProposalDocument) -> CreateProposalRequest {
    let api = &doc.frontmatter.api;
    CreateProposalRequest {
        vote_id: api.vote_id.clone(),
        title: doc.frontmatter.title.clone(),
        summary: doc.summary.clone(),
        status: api.status.clone(),
        treasury_donation_tx_hash: api.treasury_donation_tx_hash.clone(),
        meta_data: build_metadata(doc),
    }
}

fn build_metadata(doc: &ProposalDocument) -> Value {
    let api = &doc.frontmatter.api;
    json!({
        "trackRecord": doc.track_record,
        "estimatedDuration": api.estimated_duration,
        "totalBudget": doc.total_budget,
        "conversionRate": api.conversion_rate.to_string(),
        "treasuryDonationTxHash": api.treasury_donation_tx_hash,
        "treasuryDonationConfirmed": api.treasury_donation_confirmed,
        "strategyFramework": {
            "pillars": api.strategy_pillars,
            "pillarRationale": doc.pillar_rationale,
            "kpiAlignment": doc.kpi_alignment,
        },
        "proposalDetails": {
            "conversionRate": api.conversion_rate,
            "workPackages": doc.work_packages.iter().map(wp_to_json).collect::<Vec<_>>(),
        },
        "supportingInfoLinks": [],
        "treasuryRepayment": api.treasury_repayment,
        "treasuryRepaymentCircumstances": api.treasury_repayment_circumstances,
        "priorFunding": api.prior_funding,
        "administrator": api.administrator,
        "intersectAdministratorFeeConsent": api.intersect_administrator_fee_consent,
        "otherAdministrator": api.other_administrator,
        "independentAudits": api.independent_audits,
        "thirdPartyAssurer": api.third_party_assurer,
        "contractingParty": {
            "legalEntityType": api.contracting_party.legal_entity_type,
            "legalEntityName": api.contracting_party.legal_entity_name,
            "legalEntityRegistrationNumber": api.contracting_party.legal_entity_registration_number,
            "legalEntityCountry": api.contracting_party.legal_entity_country,
            "legalEntityAddressLine1": api.contracting_party.legal_entity_address_line1,
            "legalEntityAddressLine2": api.contracting_party.legal_entity_address_line2,
            "legalEntityAddressLine3": api.contracting_party.legal_entity_address_line3,
            "govtIssuedIdNumber": api.contracting_party.govt_issued_id_number,
            "countryOfResidence": api.contracting_party.country_of_residence,
            "residentialAddressLine1": api.contracting_party.residential_address_line1,
            "residentialAddressLine2": api.contracting_party.residential_address_line2,
            "residentialAddressLine3": api.contracting_party.residential_address_line3,
        },
        "primaryContact": {
            "name": api.primary_contact.name,
            "email": api.primary_contact.email,
        },
        "signatoryContact": {
            "title": api.signatory_contact.title,
            "name": api.signatory_contact.name,
            "email": api.signatory_contact.email,
            "authorization": api.signatory_contact.authorization,
        },
        "kycInfo": {
            "submitted": api.kyc_info.submitted,
            "email": api.kyc_info.email,
        },
        "proposerDetails": {
            "name": api.proposer_details.name,
            "email": api.proposer_details.email,
            "links": api.proposer_details.links,
        },
        "legalDeclarations": {
            "submitterName": api.legal_declarations.submitter_name,
            "submitterEmail": api.legal_declarations.submitter_email,
            "submitterRole": api.legal_declarations.submitter_role,
            "authorizationEntity": api.legal_declarations.authorization_entity,
            "authorizationAttestation": api.legal_declarations.authorization_attestation,
        },
    })
}

fn wp_to_json(wp: &WorkPackage) -> Value {
    json!({
        "name": wp.name,
        "summary": wp.summary,
        "initiativeType": wp.initiative_type,
        "proposalType": wp.proposal_type,
        "supportingDocuments": wp.supporting_documents.iter().map(doc_to_json).collect::<Vec<_>>(),
        "coreObjectives": wp.core_objectives,
        "expectedValue": wp.expected_value,
        "metric": wp.metric,
        "milestones": wp.milestones.iter().map(ms_to_json).collect::<Vec<_>>(),
        "budgetBreakdown": wp.budget_breakdown.iter().map(bi_to_json).collect::<Vec<_>>(),
        "noMilestonesBreakdown": false,
        "milestoneAlternative": "",
        "totalBudget": wp.total_budget,
    })
}

fn doc_to_json(d: &SupportingDoc) -> Value {
    json!({ "title": d.title, "url": d.url })
}

fn ms_to_json(m: &Milestone) -> Value {
    json!({
        "name": m.name,
        "deliverables": m.deliverables,
        "acceptanceCriteria": m.acceptance_criteria,
        "duration": m.duration,
    })
}

fn bi_to_json(b: &BudgetItem) -> Value {
    json!({
        "costCategory": b.cost_category,
        "name": b.name,
        "description": b.description,
        "quantity": b.quantity,
        "unitPrice": b.unit_price,
        "total": b.total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proposal::parser::parse_document;

    const MINIMAL: &str = r#"---
title: Budget Test
api:
  voteId: "vote1"
  status: live
  estimatedDuration: "12"
  conversionRate: "0.25"
  treasuryDonationTxHash: "txhash"
---
## Executive Summary
Summary.
## Work Package 1: Alpha
### Budget
> wpTotal: 200000
- **Team** | Resources (Labor) | 150000: Labor.
- **Infra** | Infrastructure | 50000: Infra costs.
## Budget Summary
> totalBudget: 206000
"#;

    #[test]
    fn request_has_required_top_level_fields() {
        let doc = parse_document(MINIMAL).unwrap();
        let req = build_request(&doc);
        assert_eq!(req.vote_id, "vote1");
        assert_eq!(req.title, "Budget Test");
        assert_eq!(req.status, "live");
    }

    #[test]
    fn metadata_contains_work_packages_under_proposal_details() {
        let doc = parse_document(MINIMAL).unwrap();
        let req = build_request(&doc);
        let wps = req.meta_data["proposalDetails"]["workPackages"]
            .as_array()
            .expect("workPackages should be an array");
        assert_eq!(wps.len(), 1);
        assert_eq!(wps[0]["name"], "Alpha");
    }

    #[test]
    fn budget_breakdown_totals_match_source() {
        let doc = parse_document(MINIMAL).unwrap();
        let req = build_request(&doc);
        let breakdown = req.meta_data["proposalDetails"]["workPackages"][0]["budgetBreakdown"]
            .as_array()
            .unwrap();
        assert_eq!(breakdown.len(), 2);
        let sum: u64 = breakdown
            .iter()
            .map(|b| b["total"].as_u64().unwrap_or(0))
            .sum();
        assert_eq!(sum, 200_000);
    }

    #[test]
    fn estimated_duration_serialized_as_string_in_metadata() {
        let doc = parse_document(MINIMAL).unwrap();
        let req = build_request(&doc);
        // The top-level metaData.estimatedDuration must be the string "12", not the number 12
        assert_eq!(req.meta_data["estimatedDuration"], "12");
    }

    #[test]
    fn conversion_rate_string_in_top_level_metadata() {
        let doc = parse_document(MINIMAL).unwrap();
        let req = build_request(&doc);
        // conversionRate at the top level of metaData is a string
        assert!(req.meta_data["conversionRate"].is_string());
    }

    #[test]
    fn wp_milestone_fields_present() {
        let src = r#"---
title: T
api:
  voteId: v
---
## Work Package 1: WP
### Milestones
#### Milestone 1: M1 (Duration: 4 weeks)
**Deliverables:**
- Deliver X
**Acceptance Criteria:**
- X is delivered
### Budget
> wpTotal: 10000
- **T** | Labor | 10000: Work.
## Budget Summary
> totalBudget: 10300
"#;
        let doc = parse_document(src).unwrap();
        let req = build_request(&doc);
        let ms = req.meta_data["proposalDetails"]["workPackages"][0]["milestones"]
            .as_array()
            .unwrap();
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0]["name"], "M1");
        assert!(
            ms[0]["deliverables"]
                .as_str()
                .unwrap()
                .contains("Deliver X")
        );
        assert!(
            ms[0]["acceptanceCriteria"]
                .as_str()
                .unwrap()
                .contains("X is delivered")
        );
    }
}
