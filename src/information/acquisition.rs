use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AcquisitionBudgetV1 {
    pub max_queries: u32,
    pub max_analysis_millis: u64,
}

impl Default for AcquisitionBudgetV1 {
    fn default() -> Self {
        Self {
            max_queries: 1,
            max_analysis_millis: 1_000,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AcquisitionCandidateV1 {
    pub recipe_identity: String,
    pub lower_bound_capacity_gain_us: u64,
    pub upper_bound_acquisition_cost_us: u64,
    pub declared_analysis_millis: u64,
    pub read_only: bool,
}

impl AcquisitionCandidateV1 {
    pub fn conservative_net_gain_us(&self) -> Option<u64> {
        self.lower_bound_capacity_gain_us
            .checked_sub(self.upper_bound_acquisition_cost_us)
            .filter(|gain| *gain > 0)
    }
}

pub fn select_candidate_v1(
    candidates: &[AcquisitionCandidateV1],
    budget: AcquisitionBudgetV1,
) -> Option<&AcquisitionCandidateV1> {
    if budget.max_queries == 0 {
        return None;
    }
    candidates
        .iter()
        .filter(|candidate| {
            candidate.read_only
                && candidate.declared_analysis_millis <= budget.max_analysis_millis
                && candidate.conservative_net_gain_us().is_some()
        })
        .max_by(|left, right| {
            left.conservative_net_gain_us()
                .cmp(&right.conservative_net_gain_us())
                .then_with(|| {
                    right
                        .upper_bound_acquisition_cost_us
                        .cmp(&left.upper_bound_acquisition_cost_us)
                })
                .then_with(|| right.recipe_identity.cmp(&left.recipe_identity))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planner_is_bounded_read_only_and_deterministic() {
        let candidates = vec![
            AcquisitionCandidateV1 {
                recipe_identity: "b".to_string(),
                lower_bound_capacity_gain_us: 2_000,
                upper_bound_acquisition_cost_us: 500,
                declared_analysis_millis: 100,
                read_only: true,
            },
            AcquisitionCandidateV1 {
                recipe_identity: "a".to_string(),
                lower_bound_capacity_gain_us: 2_000,
                upper_bound_acquisition_cost_us: 500,
                declared_analysis_millis: 100,
                read_only: true,
            },
            AcquisitionCandidateV1 {
                recipe_identity: "effectful".to_string(),
                lower_bound_capacity_gain_us: 10_000,
                upper_bound_acquisition_cost_us: 1,
                declared_analysis_millis: 1,
                read_only: false,
            },
        ];
        assert_eq!(
            select_candidate_v1(&candidates, AcquisitionBudgetV1::default())
                .unwrap()
                .recipe_identity,
            "a"
        );
        assert!(select_candidate_v1(
            &candidates,
            AcquisitionBudgetV1 {
                max_queries: 0,
                max_analysis_millis: 1_000,
            }
        )
        .is_none());
    }
}
