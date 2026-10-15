//! Compatibility field names over the canonical reason-specific admission rule.

pub use super::risk::{
    AdmissionAction, AdmissionFacts, AuthorityReason as B4Reason, GuardReason, NetTrust,
};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdmissionReasons {
    pub b4: BTreeSet<B4Reason>,
    pub guards: BTreeSet<GuardReason>,
}

impl AdmissionReasons {
    pub fn is_empty(&self) -> bool {
        self.b4.is_empty() && self.guards.is_empty()
    }

    pub fn admits(&self, action: AdmissionAction, facts: AdmissionFacts) -> bool {
        super::risk::admission_permits(&self.b4, &self.guards, action, facts)
    }

    pub fn describe(&self) -> String {
        if self.is_empty() {
            return "none".to_string();
        }
        let mut parts: Vec<String> = self.b4.iter().map(|r| format!("{r:?}")).collect();
        parts.extend(self.guards.iter().map(|r| format!("{r:?}")));
        parts.join(",")
    }
}
