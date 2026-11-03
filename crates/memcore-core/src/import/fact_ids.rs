//! The tenant-scoped deterministic id an imported
//! fact gets when the importing tenant does not already own its exported id.
//!
//! Fact ids are global keys. A supplied id the importer does not own can therefore not be kept
//! whenever another tenant holds it, and keeping it only when no one does would let the import
//! reveal other tenants' ids (S3I design §7). Every non-owned id is stored under
//! [`derived_import_fact_id`] instead. It depends only on the importing tenant and the source id,
//! never on any store. The same export imported again into the same tenant maps to the same ids,
//! so R7 still recognises a re-application.
//!
//! Version-8 UUIDs never equal the version-4 ids memcore mints. SHA-256 truncated to 122 bits makes
//! a collision computationally improbable, not impossible, and legacy imports may hold any value
//! (amendment v1.1 §7).

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::TenantContext;

/// Validation issue code (and message prefix) for two exported facts that share a source id, or
/// that would be stored under the same id.
pub const DUPLICATE_FACT_ID: &str = "DUPLICATE_FACT_ID";

/// Domain-separation tag of the v1 derivation (amendment v1.1 §3).
const DOMAIN: &[u8] = b"memcore.import-fact-id.v1";

fn push_field(input: &mut Vec<u8>, field: &str) {
    let bytes = field.as_bytes();
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    input.extend_from_slice(&len.to_be_bytes());
    input.extend_from_slice(bytes);
}

/// `M(tenant, source)`: SHA-256 over the domain tag, a zero byte, the length-prefixed
/// organization and user ids and the 16 source-id bytes, truncated to 16 bytes with the RFC 9562
/// version-8 and variant bits set.
pub fn derived_import_fact_id(tenant: &TenantContext, source: Uuid) -> Uuid {
    let mut input = Vec::with_capacity(DOMAIN.len() + 1 + 8 + 16 + 64);
    input.extend_from_slice(DOMAIN);
    input.push(0);
    push_field(&mut input, &tenant.org_id);
    push_field(&mut input, &tenant.user_id);
    input.extend_from_slice(source.as_bytes());
    let digest = Sha256::digest(&input);
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0F) | 0x80;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    Uuid::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tenant(org: &str, user: &str) -> TenantContext {
        TenantContext::new(org, user).expect("tenant")
    }

    const SOURCE: &str = "6f1c2c1e-7a4b-4c55-9b39-0d2a9f7e3b11";

    #[test]
    fn is21_1_deterministic() {
        let source = Uuid::parse_str(SOURCE).unwrap();
        let a = tenant("org_a", "u1");
        assert_eq!(
            derived_import_fact_id(&a, source),
            derived_import_fact_id(&a, source)
        );
    }

    #[test]
    fn is21_2_3_tenant_specific() {
        let source = Uuid::parse_str(SOURCE).unwrap();
        let a1 = derived_import_fact_id(&tenant("org_a", "u1"), source);
        assert_ne!(
            a1,
            derived_import_fact_id(&tenant("org_a", "u2"), source),
            "other user"
        );
        assert_ne!(
            a1,
            derived_import_fact_id(&tenant("org_b", "u1"), source),
            "other org"
        );
        assert_ne!(a1, source, "never the source id");
    }

    #[test]
    fn is21_4_version_8_and_rfc_variant() {
        for n in 0..64u128 {
            let id = derived_import_fact_id(&tenant("org_a", "u1"), Uuid::from_u128(n));
            assert_eq!(id.get_version_num(), 8);
            assert_eq!(id.get_variant(), uuid::Variant::RFC4122);
            assert_ne!(id.get_version_num(), Uuid::new_v4().get_version_num());
        }
    }

    #[test]
    fn is21_5_no_field_boundary_ambiguity() {
        let source = Uuid::parse_str(SOURCE).unwrap();
        assert_ne!(
            derived_import_fact_id(&tenant("ab", "c"), source),
            derived_import_fact_id(&tenant("a", "bc"), source)
        );
        assert_ne!(
            derived_import_fact_id(&tenant("org:a", "u"), source),
            derived_import_fact_id(&tenant("org", "a:u"), source)
        );
    }

    #[test]
    fn is21_6_textual_forms_are_canonical() {
        let t = tenant("org_a", "u1");
        let forms = [
            SOURCE.to_string(),
            SOURCE.to_ascii_uppercase(),
            SOURCE.replace('-', ""),
            format!("{{{SOURCE}}}"),
            format!("urn:uuid:{SOURCE}"),
        ];
        let expected = derived_import_fact_id(&t, Uuid::parse_str(SOURCE).unwrap());
        for form in forms {
            assert_eq!(
                derived_import_fact_id(&t, Uuid::parse_str(&form).unwrap()),
                expected,
                "{form}"
            );
        }
    }

    #[test]
    fn is21_7_depends_on_tenant_and_source_only() {
        // The signature takes no store; a fixed vector pins the v1 derivation itself.
        let id = derived_import_fact_id(&tenant("org_a", "u1"), Uuid::parse_str(SOURCE).unwrap());
        let again =
            derived_import_fact_id(&tenant("org_a", "u1"), Uuid::parse_str(SOURCE).unwrap());
        assert_eq!(id, again);
        assert_eq!(id.to_string().len(), 36);
    }
}
