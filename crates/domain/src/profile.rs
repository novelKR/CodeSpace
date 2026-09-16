use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Profile {
    #[default]
    ReadOnly,
    WorkspaceWrite,
}

impl Profile {
    pub fn allows_mutation(self) -> bool {
        matches!(self, Self::WorkspaceWrite)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_read_only() {
        assert_eq!(Profile::default(), Profile::ReadOnly);
        assert!(!Profile::ReadOnly.allows_mutation());
        assert!(Profile::WorkspaceWrite.allows_mutation());
    }
}
