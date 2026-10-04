// Fluence sync - Phase 6 STAGE 3: structural remote account partitioning.
//
// Mirror of the Android `AccountPartition` object. The two implementations
// MUST compute identical strings, because the account hash becomes a Drive path
// segment shared by both platforms. Any divergence splits every account
// partition on every device, so it is pinned by the same fixture literals in
// both test suites rather than discovered at runtime.
//
// Layout produced here:
//
//   appDataFolder/fluence/v1/acct-<accountHash>/agents.json
//   appDataFolder/fluence/v1/acct-<accountHash>/styles.json
//
// Rationale and the full design note live on the Android side
// (`sync/v1/AccountPartition.kt`); the short version:
//
//   * The Drive `appDataFolder` is keyed by Google identity, not by email, so a
//     single Google account can back several Fluence identities. Pooling Agents
//     and Styles at `fluence/v1/agents.json` left that data structurally
//     unowned.
//   * Partitioning makes isolation structural rather than bookkeeping-driven:
//     account A cannot name B's location, so "A silently adopts B's records"
//     stops being a policy question. No `owner_id`, no envelope change, no
//     backend.
//   * The `acct-` prefix makes the segment self-describing, and the segment
//     itself is a second, tagged derivation
//     `SHA-256("fluence/acct-path/v1\0" + accountHash)` rather than the account
//     hash itself. The account hash also appears in `syncAccount` columns,
//     `SyncMetadata` and local account filenames, so reusing it here would let
//     one observed value link a Drive folder to an exported database or a file
//     listing. Tagging the hashed *input* is what makes this domain separation;
//     prefixing the hash *output* would not.
//
// What this does not defend against is documented on `path_segment`: an
// adversary who already knows the email can still compute the path. That is
// irreducible, and the partition key still requires knowledge of the email, so
// structural isolation is unaffected.
//
// Fail-closed: a hash that is not exactly 64 lowercase hex characters yields
// `None` everywhere, and callers must then neither create a folder nor read or
// write a file. Because a validated hash cannot contain a separator or a dot,
// path construction cannot escape `fluence/v1/`.

use crate::account_scope::valid_account_hash;
use sha2::Digest;

/// Domain separator for the per-account folder segment.
pub const FOLDER_PREFIX: &str = "acct-";

/// Tag mixed into the hashed input to derive the path segment.
///
/// MUST stay byte-identical to the Android constant
/// `AccountPartition.PATH_DOMAIN_TAG`, including the NUL terminator. A NUL
/// separator is used because it cannot occur inside a hex hash, so the tagged
/// input is unambiguous.
pub const PATH_DOMAIN_TAG: &str = "fluence/acct-path/v1";

/// Fixed path segments between `appDataFolder` and the partition folder.
pub const ROOT: &str = "fluence/v1";

/// Leaf file name for the Agents domain (additive, partitioned).
pub const AGENTS_FILE: &str = "agents.json";

/// Leaf file name for the Styles domain (additive, partitioned).
pub const STYLES_FILE: &str = "styles.json";

/// Domains stored inside a per-account partition.
///
/// Only the additive Agent/Style domains are partitioned. Adding another name
/// here would move an existing file and break old clients.
pub fn is_partitioned(file_name: &str) -> bool {
    file_name == AGENTS_FILE || file_name == STYLES_FILE
}

/// The value that actually appears in the Drive path.
///
/// Deliberately a *second* derivation, distinct from the account hash, so the
/// path segment is not the same value used in `syncAccount`, `SyncMetadata` and
/// local account filenames. Tagging the hashed *input* is what makes this
/// domain separation: a value seen in a Drive path is then a value that exists
/// nowhere else, so it cannot be correlated with the account hash seen in local
/// storage.
///
/// This does not, and cannot, defend against an adversary who already knows the
/// email: every device of an account must converge on one deterministic path and
/// the client has no shared secret, so determinism and secrecy cannot both be
/// had. See the note on the Android side for the full statement of this limit.
pub fn path_segment(account_hash: &str) -> Option<String> {
    if !valid_account_hash(account_hash) {
        return None;
    }
    let mut tagged = Vec::from(PATH_DOMAIN_TAG.as_bytes());
    tagged.push(0);
    tagged.extend_from_slice(account_hash.as_bytes());
    Some(hex::encode(sha2::Sha256::digest(&tagged)))
}

/// Partition folder name for `account_hash`, or `None` when it is not a
/// well-formed account hash. `None` is the fail-closed signal.
pub fn folder_name(account_hash: &str) -> Option<String> {
    path_segment(account_hash).map(|segment| format!("{}{}", FOLDER_PREFIX, segment))
}

/// Leaf file name, independent of partitioning.
pub fn plain_file_name(file_name: &str) -> &str {
    file_name
}

/// Canonical path of a domain file relative to `appDataFolder`, or `None` when a
/// partitioned domain has no usable account hash.
///
/// This is the cross-platform fixture vector and the single source of truth both
/// platforms are tested against.
pub fn relative_path(file_name: &str, account_hash: &str) -> Option<String> {
    if is_partitioned(file_name) {
        let folder = folder_name(account_hash)?;
        Some(format!("{}/{}/{}", ROOT, folder, file_name))
    } else {
        Some(format!("{}/{}", ROOT, plain_file_name(file_name)))
    }
}

/// The account partition a device may address, or `None` when signed out or when
/// the token-derived identity is not yet resolved.
pub fn partition_for(authenticated_account_hash: &str) -> Option<String> {
    folder_name(authenticated_account_hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// STAGE 1 cross-platform fixture, independently computed and asserted
    /// identically in `sync/metadata.rs` and on Android in `DriveIdentityTest`.
    const CANONICAL_EMAIL: &str = "test@example.com";
    const CANONICAL_HASH: &str =
        "973dfe463ec85785f5f95af5ba3906eedb2d931c24e69824a89ea65dba4e813b";
    /// SHA-256("fluence/acct-path/v1\0" + CANONICAL_HASH). Independently
    /// computed and asserted identically on Android in `AccountPartitionTest`.
    const CANONICAL_SEGMENT: &str =
        "9d7f994b0d8cadd9d97315553dbb81c64fb0d9d53c702976aafd6a65069c12e2";

    fn alice_hash() -> String {
        crate::sync::metadata::account_hash_from_email("alice@example.com")
    }
    fn bob_hash() -> String {
        crate::sync::metadata::account_hash_from_email("bob@example.com")
    }

    // -- canonical path fixtures (byte-identical to the Android suite) ----

    #[test]
    fn agents_live_in_the_account_partition() {
        assert_eq!(
            Some(format!("fluence/v1/acct-{}/agents.json", CANONICAL_SEGMENT)),
            relative_path(AGENTS_FILE, CANONICAL_HASH)
        );
    }

    #[test]
    fn styles_live_in_the_account_partition() {
        assert_eq!(
            Some(format!("fluence/v1/acct-{}/styles.json", CANONICAL_SEGMENT)),
            relative_path(STYLES_FILE, CANONICAL_HASH)
        );
    }

    #[test]
    fn the_path_segment_is_a_separately_derived_value_not_the_account_hash() {
        // The folder segment must NOT be the account hash. The account hash
        // appears in `syncAccount` columns, `SyncMetadata` and local account
        // filenames, so reusing it here would let one observed value link a
        // Drive folder to an exported database or a file listing.
        let segment = path_segment(CANONICAL_HASH).unwrap();
        assert_ne!(CANONICAL_HASH, segment);
        assert_eq!(64, segment.len());
        assert!(segment
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    }

    #[test]
    fn the_path_segment_is_derived_from_the_tagged_input() {
        // Documented derivation: SHA-256("fluence/acct-path/v1\0" + accountHash)
        assert_eq!(Some(CANONICAL_SEGMENT), path_segment(CANONICAL_HASH).as_deref());
        assert_eq!(
            Some(format!("acct-{}", CANONICAL_SEGMENT)),
            folder_name(CANONICAL_HASH)
        );
    }

    #[test]
    fn two_accounts_derive_two_unrelated_path_segments() {
        assert_ne!(path_segment(&alice_hash()), path_segment(&bob_hash()));
    }

    #[test]
    fn a_malformed_account_hash_yields_no_path_segment() {
        for h in ["", "garbage", "0123456789abcdef", "not-a-hash"] {
            assert_eq!(None, path_segment(h), "must reject {:?}", h);
        }
    }

    #[test]
    fn agents_and_styles_share_one_partition_folder_per_account() {
        let a = relative_path(AGENTS_FILE, CANONICAL_HASH).unwrap();
        let s = relative_path(STYLES_FILE, CANONICAL_HASH).unwrap();
        let strip = |p: &str| p.rsplit_once('/').unwrap().0.to_string();
        assert_eq!(strip(&a), strip(&s));
    }

    // -- existing domains are NOT moved ------------------------------------

    #[test]
    fn the_four_existing_domains_keep_their_flat_paths() {
        for name in crate::sync::drive::DOMAIN_FILES {
            let expected = format!("fluence/v1/{}", name);
            assert_eq!(Some(expected.clone()), relative_path(name, CANONICAL_HASH));
            assert_eq!(Some(expected), relative_path(name, &bob_hash()));
        }
    }

    #[test]
    fn flat_domains_are_not_partitioned() {
        for name in crate::sync::drive::DOMAIN_FILES {
            assert!(!is_partitioned(name), "{} must not be partitioned", name);
        }
        assert!(is_partitioned(AGENTS_FILE));
        assert!(is_partitioned(STYLES_FILE));
    }

    #[test]
    fn no_existing_domain_file_was_moved() {
        // The frozen four stay directly inside `v1` (two separators), never
        // under a partition folder, or existing installs lose their data.
        for name in crate::sync::drive::DOMAIN_FILES {
            let p = relative_path(name, CANONICAL_HASH).unwrap();
            assert_eq!(2, p.matches('/').count(), "{} must stay directly inside v1", name);
        }
    }

    // -- cross-account isolation -------------------------------------------

    #[test]
    fn different_accounts_produce_different_partitions() {
        assert_ne!(
            relative_path(AGENTS_FILE, &alice_hash()),
            relative_path(AGENTS_FILE, &bob_hash())
        );
    }

    #[test]
    fn a_cannot_name_bs_partition() {
        let a_agents = relative_path(AGENTS_FILE, &alice_hash()).unwrap();
        let a_styles = relative_path(STYLES_FILE, &alice_hash()).unwrap();
        let b_agents = relative_path(AGENTS_FILE, &bob_hash()).unwrap();
        let b_styles = relative_path(STYLES_FILE, &bob_hash()).unwrap();
        for a in [&a_agents, &a_styles] {
            for b in [&b_agents, &b_styles] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn a_single_account_never_splits_its_own_agents_across_partitions() {
        let first = relative_path(AGENTS_FILE, &alice_hash()).unwrap();
        for _ in 0..5 {
            assert_eq!(first, relative_path(AGENTS_FILE, &alice_hash()).unwrap());
        }
    }

    // -- fail-closed on a malformed / missing hash -------------------------

    #[test]
    fn an_unusable_hash_yields_no_path_for_partitioned_domains() {
        let alice = alice_hash();
        let bad = vec![
            String::new(),
            "   ".to_string(),
            alice[..63].to_string(),       // 63 chars
            format!("{}0", alice),          // 65 chars
            alice[..16].to_string(),        // the legacy 16-hex form
            alice.to_uppercase(),           // uppercase is not canonical
            "not-a-hash".to_string(),
            format!("g{}", &alice[1..]),    // out-of-range hex char
        ];
        for h in &bad {
            assert_eq!(None, folder_name(h), "must reject {:?}", h);
            assert_eq!(None, relative_path(AGENTS_FILE, h), "must reject {:?}", h);
            assert_eq!(None, relative_path(STYLES_FILE, h), "must reject {:?}", h);
            assert_eq!(None, partition_for(h), "must reject {:?}", h);
        }
    }

    #[test]
    fn a_malformed_hash_does_not_disable_the_flat_domains() {
        // dictionary/settings carry no account-scoped Agents/Styles payload and
        // must stay syncable.
        assert_eq!(
            Some("fluence/v1/dictionary.json".to_string()),
            relative_path("dictionary.json", "garbage")
        );
    }

    #[test]
    fn the_legacy_sixteen_hex_hash_is_rejected() {
        // Guards a specific regression: an early build used a 16-char hash.
        // Accepting it would address a different partition than the account uses.
        let short = &alice_hash()[..16];
        assert!(!valid_account_hash(short));
        assert_eq!(None, folder_name(short));
    }

    // -- path traversal containment ----------------------------------------

    #[test]
    fn a_hash_cannot_escape_the_app_data_folder() {
        let traversal = vec![
            "../../etc/passwd".to_string(),
            "..\\..\\windows\\system32".to_string(),
            "acct-../../v1/dictionary.json".to_string(),
            format!("./{}", alice_hash()),
            format!("{}/agents.json", alice_hash()),
            format!("{}\\styles.json", alice_hash()),
            "....//....//x".to_string(),
        ];
        for t in &traversal {
            assert_eq!(None, folder_name(t), "must not produce a folder: {}", t);
            assert_eq!(None, relative_path(AGENTS_FILE, t), "must not produce a path: {}", t);
        }
    }

    #[test]
    fn every_generated_path_stays_inside_the_v1_root() {
        for h in [alice_hash(), bob_hash(), CANONICAL_HASH.to_string()] {
            for name in [AGENTS_FILE, STYLES_FILE] {
                let p = relative_path(name, &h).unwrap();
                assert!(p.starts_with("fluence/v1/"), "{}", p);
                assert!(!p.contains(".."), "{}", p);
                assert!(!p.contains('\\'), "{}", p);
            }
        }
    }

    #[test]
    fn the_partition_segment_is_exactly_the_prefix_plus_a_64_hex_hash() {
        let folder = folder_name(CANONICAL_HASH).unwrap();
        let hash = folder.strip_prefix(FOLDER_PREFIX).unwrap();
        assert_eq!(64, hash.len());
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    }

    // -- old-client compatibility ------------------------------------------

    #[test]
    fn an_old_client_querying_v1_by_exact_name_cannot_match_a_partition() {
        // An old client lists `fluence/v1` for a file named exactly
        // "agents.json". The new location is a FOLDER named "acct-<hash>", so
        // the name never matches and the old client sees nothing new.
        let partition = relative_path(AGENTS_FILE, CANONICAL_HASH).unwrap();
        let old_client_query = "fluence/v1/agents.json";
        assert_ne!(old_client_query, partition);
        assert!(partition.matches('/').count() > old_client_query.matches('/').count());
        assert!(partition.ends_with("/agents.json"));
    }

    #[test]
    fn the_four_frozen_files_stay_reachable_by_an_old_client() {
        for name in crate::sync::drive::DOMAIN_FILES {
            let p = relative_path(name, CANONICAL_HASH).unwrap();
            assert_eq!(2, p.matches('/').count(), "{} must remain directly inside v1", name);
        }
    }

    #[test]
    fn an_old_windows_client_does_not_treat_the_partition_as_a_domain_file() {
        // `DOMAIN_FILES` is the allowlist an existing client uses to decide
        // what to read. Adding agents/styles to it would be the break; the
        // partition folder must never appear there.
        assert!(!crate::sync::drive::DOMAIN_FILES.contains(&AGENTS_FILE));
        assert!(!crate::sync::drive::DOMAIN_FILES.contains(&STYLES_FILE));
        for name in crate::sync::drive::DOMAIN_FILES {
            assert!(!is_partitioned(name));
        }
    }

    // -- hash derivation is shared with the partition path ------------------

    #[test]
    fn the_partition_uses_the_shared_hash_derivation() {
        let derived = crate::sync::metadata::account_hash_from_email(CANONICAL_EMAIL);
        assert_eq!(CANONICAL_HASH, derived);
        assert_eq!(
            Some(format!("fluence/v1/acct-{}/agents.json", path_segment(&derived).unwrap())),
            relative_path(AGENTS_FILE, &derived)
        );
    }
}
