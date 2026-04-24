//! KV cache namespace key construction.
//!
//! Implements the tenant isolation scheme defined in design §5.2.
//! Namespace key format: `{tenant_id}:{sensitivity}:{agent_type}:{prefix_hash_hex}`
//!
//! Issue: alternatefutures/admin#132

use sha2::{Digest, Sha256};
use crate::{KvNamespaceKey, SensitivityLevel};

/// Build a KV cache namespace key for a given request context.
///
/// The key ensures that KV cache blocks from one tenant are never served to another,
/// even if their system prompts hash-collide (FM-20 protection).
pub fn build_namespace_key(
    tenant_id: &str,
    sensitivity: SensitivityLevel,
    agent_type: &str,
    system_prompt_prefix: &str,
) -> KvNamespaceKey {
    let mut hasher = Sha256::new();
    hasher.update(system_prompt_prefix.as_bytes());
    let prefix_hash = hasher.finalize().to_vec();

    KvNamespaceKey {
        tenant_id: tenant_id.to_string(),
        sensitivity: sensitivity as i32,
        agent_type: agent_type.to_string(),
        prefix_hash,
    }
}

/// Render the namespace key as a string prefix for cache lookup.
///
/// This string is prepended to all cache keys before they reach vLLM's
/// prefix cache, ensuring tenant isolation (see design §5.2).
pub fn namespace_key_to_prefix(key: &KvNamespaceKey) -> String {
    let sensitivity = SensitivityLevel::try_from(key.sensitivity)
        .unwrap_or(SensitivityLevel::Unspecified);
    format!(
        "{}:{}:{}:{}",
        key.tenant_id,
        sensitivity_label(sensitivity),
        key.agent_type,
        hex::encode(&key.prefix_hash),
    )
}

fn sensitivity_label(s: SensitivityLevel) -> &'static str {
    match s {
        SensitivityLevel::Public       => "PUBLIC",
        SensitivityLevel::Internal     => "INTERNAL",
        SensitivityLevel::Sensitive    => "SENSITIVE",
        SensitivityLevel::Restricted   => "RESTRICTED",
        SensitivityLevel::Unspecified  => "UNSPECIFIED",
    }
}

// COORDINATION NOTE (argus #133):
// RESTRICTED tasks use namespace prefix "RESTRICTED:{tenant_id}:..."
// These keys must only appear on the Tier A RESTRICTED POD vLLM instance.
// The InferenceRouter must NEVER inject a RESTRICTED namespace key into the
// OPEN POOL vLLM. The ForwardToRestrictedPod RPC enforces this boundary.
// See design §5.3 and §6.3.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_tenants_produce_different_prefixes() {
        let key_a = build_namespace_key("tenant_a", SensitivityLevel::Internal, "orchestrator", "system prompt");
        let key_b = build_namespace_key("tenant_b", SensitivityLevel::Internal, "orchestrator", "system prompt");
        // Same system prompt, same agent type — different tenant → different prefix
        assert_ne!(
            namespace_key_to_prefix(&key_a),
            namespace_key_to_prefix(&key_b),
        );
    }

    #[test]
    fn restricted_prefix_contains_restricted_label() {
        let key = build_namespace_key("tenant_xyz", SensitivityLevel::Restricted, "orchestrator", "classified prompt");
        let prefix = namespace_key_to_prefix(&key);
        assert!(prefix.starts_with("tenant_xyz:RESTRICTED:"));
    }
}
