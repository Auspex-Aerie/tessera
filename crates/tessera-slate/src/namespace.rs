//! Namespace identification — BLAKE3-derived region handle.
//!
//! Mirrors the other Tessera primitives: a human-readable `description`
//! is hashed with BLAKE3 to a deterministic handle, so two peers that
//! pass the same description attach to the same SHM region with no manual
//! coordination. The SHM name prefix (`/tessera-slate-`) keeps a Slate
//! region from colliding with a Ring / Pool / Channel region derived from
//! the same description.
//!
//! This is deliberately a private copy, not a shared dependency: Tessera
//! primitives do not depend on one another (only layer-2 services like
//! Sink compose primitives). A shared `tessera-namespace` crate would be
//! a public surface coupling every primitive to it.

use blake3::Hasher;

/// 128-bit prefix of BLAKE3(description), encoded as 32 hex chars and
/// used as the POSIX SHM region name suffix.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct NamespaceHandle {
    /// First 16 bytes of BLAKE3(description).
    digest_prefix: [u8; 16],
    /// Full 32-byte BLAKE3 digest. Stored in the SHM global header so
    /// attachers can cross-verify their description against the creator's.
    full_digest: [u8; 32],
}

impl NamespaceHandle {
    /// Derive a namespace handle from the operator-facing description.
    pub fn derive(description: &str) -> Self {
        let mut h = Hasher::new();
        h.update(description.as_bytes());
        let full = h.finalize();
        let full_digest: [u8; 32] = *full.as_bytes();
        let mut digest_prefix = [0u8; 16];
        digest_prefix.copy_from_slice(&full_digest[..16]);
        Self {
            digest_prefix,
            full_digest,
        }
    }

    /// Full BLAKE3 digest for header storage / cross-verification.
    pub fn full_digest(&self) -> [u8; 32] {
        self.full_digest
    }

    /// POSIX SHM region name (`/tessera-slate-<hex>`).
    pub fn shm_name(&self) -> String {
        let mut out = String::from("/tessera-slate-");
        for byte in &self.digest_prefix {
            use core::fmt::Write;
            // Safe: writing to a String never fails.
            write!(&mut out, "{byte:02x}").unwrap();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_description_derives_same_handle() {
        let a = NamespaceHandle::derive("my-app/metrics");
        let b = NamespaceHandle::derive("my-app/metrics");
        assert_eq!(a.full_digest(), b.full_digest());
        assert_eq!(a.shm_name(), b.shm_name());
    }

    #[test]
    fn different_descriptions_derive_different_handles() {
        let a = NamespaceHandle::derive("my-app/metrics");
        let b = NamespaceHandle::derive("my-app/other");
        assert_ne!(a.full_digest(), b.full_digest());
        assert_ne!(a.shm_name(), b.shm_name());
    }

    #[test]
    fn shm_name_has_expected_shape() {
        let h = NamespaceHandle::derive("test");
        let name = h.shm_name();
        assert!(name.starts_with("/tessera-slate-"));
        assert_eq!(name.len(), "/tessera-slate-".len() + 32);
        let hex = &name["/tessera-slate-".len()..];
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn slate_name_does_not_collide_with_ring_prefix() {
        // Same description, different prefix: a Slate and a Ring region for
        // the same description can coexist without a name collision.
        let h = NamespaceHandle::derive("shared-description");
        assert!(h.shm_name().starts_with("/tessera-slate-"));
        assert!(!h.shm_name().starts_with("/tessera-ring-"));
    }
}
