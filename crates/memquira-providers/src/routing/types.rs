use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderCapability {
    Llm,
    Embedding,
    Summarization,
}

impl fmt::Display for ProviderCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Llm => write!(f, "llm"),
            Self::Embedding => write!(f, "embedding"),
            Self::Summarization => write!(f, "summarization"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderId {
    pub name: String,
    pub capability: ProviderCapability,
}

impl ProviderId {
    pub fn new(name: impl Into<String>, capability: ProviderCapability) -> Self {
        Self {
            name: name.into(),
            capability,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCallContext {
    pub operation_name: &'static str,
    pub provider_id: ProviderId,
}

impl ProviderCallContext {
    pub fn new(operation_name: &'static str, provider_id: ProviderId) -> Self {
        Self {
            operation_name,
            provider_id,
        }
    }
}

pub fn circuit_key(provider_id: &ProviderId, operation_name: &str) -> String {
    format!(
        "{}:{}:{}",
        provider_id.capability, provider_id.name, operation_name
    )
}
