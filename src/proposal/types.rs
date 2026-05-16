use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

fn default_conversion_rate() -> f64 {
    0.25
}

fn deserialize_f64_or_string<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StrOrF64 {
        F(f64),
        S(String),
    }
    match StrOrF64::deserialize(d)? {
        StrOrF64::F(v) => Ok(v),
        StrOrF64::S(s) => s.parse().map_err(serde::de::Error::custom),
    }
}

// ── Frontmatter (YAML in the markdown file) ───────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct Frontmatter {
    pub title: String,
    pub api: ApiConfig,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ApiConfig {
    pub vote_id: String,
    pub proposal_id: Option<String>,
    pub status: String,
    pub estimated_duration: String,
    #[serde(
        deserialize_with = "deserialize_f64_or_string",
        default = "default_conversion_rate"
    )]
    pub conversion_rate: f64,
    pub treasury_donation_tx_hash: String,
    pub treasury_donation_confirmed: bool,
    pub strategy_pillars: Vec<String>,
    pub treasury_repayment: String,
    pub treasury_repayment_circumstances: String,
    pub prior_funding: String,
    pub administrator: String,
    pub intersect_administrator_fee_consent: bool,
    pub other_administrator: String,
    pub independent_audits: String,
    pub third_party_assurer: String,
    pub contracting_party: ContractingParty,
    pub primary_contact: NameEmail,
    pub signatory_contact: SignatoryContact,
    pub kyc_info: KycInfo,
    pub proposer_details: ProposerDetails,
    pub legal_declarations: LegalDeclarations,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            vote_id: String::new(),
            proposal_id: None,
            status: "draft".into(),
            estimated_duration: "12".into(),
            conversion_rate: 0.25,
            treasury_donation_tx_hash: String::new(),
            treasury_donation_confirmed: false,
            strategy_pillars: vec![],
            treasury_repayment: "Yes".into(),
            treasury_repayment_circumstances: String::new(),
            prior_funding: String::new(),
            administrator: "Intersect".into(),
            intersect_administrator_fee_consent: true,
            other_administrator: String::new(),
            independent_audits: "TBD".into(),
            third_party_assurer: "TBD".into(),
            contracting_party: Default::default(),
            primary_contact: Default::default(),
            signatory_contact: Default::default(),
            kyc_info: Default::default(),
            proposer_details: Default::default(),
            legal_declarations: Default::default(),
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ContractingParty {
    pub legal_entity_type: String,
    pub legal_entity_name: String,
    pub legal_entity_registration_number: String,
    pub legal_entity_country: String,
    pub legal_entity_address_line1: String,
    pub legal_entity_address_line2: String,
    pub legal_entity_address_line3: String,
    pub govt_issued_id_number: String,
    pub country_of_residence: String,
    pub residential_address_line1: String,
    pub residential_address_line2: String,
    pub residential_address_line3: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct NameEmail {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SignatoryContact {
    pub title: String,
    pub name: String,
    pub email: String,
    pub authorization: bool,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct KycInfo {
    pub submitted: String,
    pub email: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ProposerDetails {
    pub name: String,
    pub email: String,
    pub links: Vec<Value>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LegalDeclarations {
    pub submitter_name: String,
    pub submitter_email: String,
    pub submitter_role: String,
    pub authorization_entity: bool,
    pub authorization_attestation: bool,
}

// ── Parsed work package ───────────────────────────────────────────────────────

#[derive(Debug)]
pub struct WorkPackage {
    pub name: String,
    pub summary: String,
    pub initiative_type: String,
    pub proposal_type: String,
    pub supporting_documents: Vec<SupportingDoc>,
    pub core_objectives: Vec<String>,
    pub expected_value: String,
    pub metric: String,
    pub milestones: Vec<Milestone>,
    pub budget_breakdown: Vec<BudgetItem>,
    pub total_budget: u64,
}

#[derive(Debug, Serialize)]
pub struct SupportingDoc {
    pub title: String,
    pub url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Milestone {
    pub name: String,
    pub deliverables: String,
    pub acceptance_criteria: String,
    pub duration: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetItem {
    pub cost_category: String,
    pub name: String,
    pub description: String,
    pub quantity: u32,
    pub unit_price: u64,
    pub total: u64,
}

// ── Parsed proposal document ──────────────────────────────────────────────────

#[derive(Debug)]
pub struct ProposalDocument {
    pub frontmatter: Frontmatter,
    pub summary: String,
    pub track_record: String,
    pub pillar_rationale: String,
    pub kpi_alignment: String,
    pub work_packages: Vec<WorkPackage>,
    pub total_budget: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion_rate_float_form() {
        let yaml = "voteId: v\nconversionRate: 0.25\n";
        let cfg: ApiConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.conversion_rate, 0.25);
    }

    #[test]
    fn conversion_rate_string_form() {
        let yaml = "voteId: v\nconversionRate: \"0.25\"\n";
        let cfg: ApiConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.conversion_rate, 0.25);
    }

    #[test]
    fn conversion_rate_defaults_when_absent() {
        let yaml = "voteId: v\n";
        let cfg: ApiConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.conversion_rate, 0.25);
    }

    #[test]
    fn conversion_rate_string_invalid_errors() {
        let yaml = "voteId: v\nconversionRate: \"not_a_number\"\n";
        assert!(serde_yaml::from_str::<ApiConfig>(yaml).is_err());
    }
}
