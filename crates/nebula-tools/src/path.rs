//! The single path-resolution and confinement routine (Requirement 7, `Path_Resolver`).
//!
//! Every file built-in and `shell.run` path check funnels through [`resolve`] so that
//! path-escape protection lives in exactly one place and is tested in exactly one place
//! (Requirement 7.1). The routine fully resolves a candidate path — expanding `.`/`..`,
//! following symbolic links and NTFS junctions, and canonicalizing the absolute form — before
//! it decides whether the result is confined to the `Worktree_Root` and off the `Retired_Drive`.
//!
//! Two rejections are distinguished so callers (and property tests) can assert the governing
//! basis: [`RejectReason::RetiredDrive`] and [`RejectReason::WorktreeEscape`]. The retired-drive
//! check runs **first** so it takes precedence over a worktree escape (Requirement 2.9, 7.6),
//! including for read operations and link/junction targets.

use std::io;
use std::path::{Component, Path, PathBuf, Prefix};

/// Why the [`Path_Resolver`](self) rejected a candidate path.
///
/// Distinct values let callers map each basis to the right diagnostic and let tests assert that
/// the retired-drive basis is reported in preference to a worktree escape (Requirement 7.5, 7.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    /// The resolved path is not contained within the `Worktree_Root` (Requirement 7.5).
    WorktreeEscape,
    /// The resolved path lies on the `Retired_Drive` (Requirement 7.6). Takes precedence over
    /// [`WorktreeEscape`](Self::WorktreeEscape) (Requirement 2.9).
    RetiredDrive,
}

/// The outcome of resolving a candidate path through [`resolve`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolved {
    /// The path is permitted: inside the `Worktree_Root` and off the `Retired_Drive`
    /// (Requirement 7.4). Carries the fully resolved, canonical path.
    Permitted(PathBuf),
    /// The path is rejected, with the governing [`RejectReason`] (retired-drive takes precedence,
    /// Requirement 2.9).
    Rejected(RejectReason),
}

/// Resolve and confine a candidate path (Requirement 7, the single `Path_Resolver`).
///
/// `candidate` may be relative — in which case it is joined onto `worktree_root` — or absolute, in
/// which case it is taken as-is. `worktree_root` is the confinement root (`Worktree_Root`), and
/// `retired_drive` is the configured `resources.retired_drive` (default `"C:"`); only its drive
/// letter is compared, case-insensitively.
///
/// The candidate is canonicalized so that `.`/`..`, symbolic links, and NTFS junctions are all
/// resolved to their real target before any comparison (Requirement 7.2). A candidate that does
/// not yet exist (for example the target of an `fs.write` that creates a new file) is resolved by
/// canonicalizing its nearest existing ancestor and re-joining the lexically normalized remaining
/// segments (Requirement 7.3).
///
/// # Errors
///
/// Returns an [`io::Error`] only for I/O failures encountered while canonicalizing the worktree
/// root or an existing path component (for example a permission error). A path that simply does
/// not exist is **not** an error — it is resolved via its nearest existing ancestor. Confinement
/// failures are reported as [`Resolved::Rejected`], not as errors.
pub fn resolve(
    candidate: &Path,
    worktree_root: &Path,
    retired_drive: &str,
) -> io::Result<Resolved> {
    // 1. Canonicalize the worktree root once, up front (yields the `\\?\` extended-length form).
    let canonical_root = std::fs::canonicalize(worktree_root)?;

    // 2. Make the candidate absolute: relative candidates join onto the worktree root, absolute
    //    candidates are taken as-is. `.`/`..` are left for canonicalization / normalization.
    let absolute: PathBuf = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        worktree_root.join(candidate)
    };

    // 3 & 4. Canonicalize, resolving `..`/`.`, symlinks, and junctions. For a path that does not
    //        exist, fall back to the nearest existing ancestor and rejoin the normalized tail.
    let resolved = canonicalize_or_nearest_ancestor(&absolute)?;

    // 5. Retired-drive check FIRST (precedence over worktree escape — Requirement 2.9, 7.6).
    if on_retired_drive(&resolved, retired_drive) {
        return Ok(Resolved::Rejected(RejectReason::RetiredDrive));
    }

    // 6. Component-wise containment check against the canonical root (not a raw string prefix, so
    //    `C:\work` never matches `C:\work-other`). A leading `\\?\` is stripped from both sides.
    if !is_contained(&resolved, &canonical_root) {
        return Ok(Resolved::Rejected(RejectReason::WorktreeEscape));
    }

    // 7. Otherwise permitted (Requirement 7.4).
    Ok(Resolved::Permitted(resolved))
}

/// Canonicalize `path`, or — when `path` does not exist — canonicalize its nearest existing
/// ancestor and re-attach the lexically normalized remaining segments (Requirement 7.3).
fn canonicalize_or_nearest_ancestor(path: &Path) -> io::Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(canonical) => Ok(canonical),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            // Collect trailing non-existent segments while walking up to an existing ancestor.
            // The remainder is normalized lexically so any `..` is applied against the ancestor
            // rather than being able to climb above it after canonicalization.
            let mut remainder: Vec<Component<'_>> = Vec::new();
            let mut current = path;
            loop {
                // No existing ancestor at all (e.g. a bare, non-existent prefix) → nothing to
                // canonicalize against; fall back to a lexical normalization of the whole path.
                let Some(parent) = current.parent() else {
                    return Ok(normalize_lexically(path));
                };
                // Record the final component of `current` as part of the non-existent tail.
                if let Some(last) = current.components().next_back() {
                    remainder.push(last);
                }
                match std::fs::canonicalize(parent) {
                    Ok(mut base) => {
                        // Re-attach the tail in original order, normalizing `.`/`..` lexically.
                        for component in remainder.iter().rev() {
                            match component {
                                Component::ParentDir => {
                                    base.pop();
                                }
                                Component::CurDir => {}
                                other => base.push(other.as_os_str()),
                            }
                        }
                        return Ok(base);
                    }
                    Err(inner) if inner.kind() == io::ErrorKind::NotFound => {
                        current = parent;
                    }
                    Err(inner) => return Err(inner),
                }
            }
        }
        Err(err) => Err(err),
    }
}

/// Lexically normalize a path (resolve `.`/`..` textually) without touching the filesystem. Used
/// only as a last-resort fallback when no ancestor of a non-existent path exists.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Whether `path`'s drive letter case-insensitively equals that of `retired_drive`
/// (Requirement 7.6). A path with no drive letter is treated as not on the retired drive.
fn on_retired_drive(path: &Path, retired_drive: &str) -> bool {
    match (drive_letter(path), drive_letter_of_str(retired_drive)) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(&b),
        _ => false,
    }
}

/// Extract the drive letter from a path's prefix component (e.g. `C` from `C:\...` or `\\?\C:\...`).
fn drive_letter(path: &Path) -> Option<char> {
    match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(byte) | Prefix::VerbatimDisk(byte) => Some(byte as char),
            _ => None,
        },
        _ => None,
    }
}

/// Extract a drive letter from a configured string such as `"C:"`, `"c"`, or `"C:\\"`.
fn drive_letter_of_str(value: &str) -> Option<char> {
    value.chars().next().filter(char::is_ascii_alphabetic)
}

/// Component-wise containment: is `path` the canonical root itself or a descendant of it?
///
/// A leading `\\?\` verbatim prefix is stripped from both sides so a canonicalized child
/// (always `\\?\`-prefixed) compares on equal footing with the root. Comparison is component by
/// component, so `C:\work` never matches `C:\work-other`, and is case-insensitive on the ASCII
/// range to match Windows path semantics.
fn is_contained(path: &Path, root: &Path) -> bool {
    let mut root_components = normalized_components(root);
    let mut path_components = normalized_components(path);

    loop {
        match root_components.next() {
            None => return true, // consumed the whole root; `path` is the root or below it
            Some(root_component) => match path_components.next() {
                None => return false, // path is a proper prefix of root → not contained
                Some(path_component) => {
                    if !components_match(&root_component, &path_component) {
                        return false;
                    }
                }
            },
        }
    }
}

/// Normalize a path into comparable component strings, dropping any leading `\\?\` verbatim marker
/// so the two sides of a containment check align.
fn normalized_components(path: &Path) -> impl Iterator<Item = String> + '_ {
    path.components().filter_map(|component| match component {
        // Fold the various prefix shapes to a bare drive letter so `\\?\C:` and `C:` match.
        Component::Prefix(prefix) => match prefix.kind() {
            Prefix::Disk(byte) | Prefix::VerbatimDisk(byte) => {
                Some((byte as char).to_ascii_uppercase().to_string())
            }
            _ => prefix
                .as_os_str()
                .to_str()
                .map(|s| s.trim_start_matches(r"\\?\").to_string()),
        },
        Component::RootDir | Component::CurDir => None,
        Component::ParentDir => Some("..".to_string()),
        Component::Normal(part) => part.to_str().map(ToString::to_string),
    })
}

/// Two path components match if equal case-insensitively (Windows path semantics).
fn components_match(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

#[cfg(all(test, windows))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn root_drive_letter(path: &Path) -> char {
        drive_letter(&std::fs::canonicalize(path).expect("canonicalize")).expect("drive letter")
    }

    #[test]
    fn permits_a_file_inside_the_worktree() {
        let dir = TempDir::new().expect("tempdir");
        let file = dir.path().join("inside.txt");
        fs::write(&file, b"hi").expect("write");

        // Pick a retired drive that is not the temp drive so we exercise the permit path.
        let resolved = resolve(Path::new("inside.txt"), dir.path(), "Q:").expect("resolve");
        match resolved {
            Resolved::Permitted(p) => {
                assert!(is_contained(
                    &p,
                    &std::fs::canonicalize(dir.path()).expect("canon")
                ));
            }
            other => panic!("expected Permitted, got {other:?}"),
        }
    }

    #[test]
    fn rejects_parent_escape() {
        let dir = TempDir::new().expect("tempdir");
        let resolved = resolve(Path::new(r"..\..\outside.txt"), dir.path(), "Q:").expect("resolve");
        assert_eq!(resolved, Resolved::Rejected(RejectReason::WorktreeEscape));
    }

    #[test]
    fn rejects_absolute_path_outside_worktree() {
        let dir = TempDir::new().expect("tempdir");
        let other = TempDir::new().expect("tempdir2");
        let outside = other.path().join("x.txt");
        let resolved = resolve(&outside, dir.path(), "Q:").expect("resolve");
        assert_eq!(resolved, Resolved::Rejected(RejectReason::WorktreeEscape));
    }

    #[test]
    fn retired_drive_takes_precedence_over_escape() {
        let dir = TempDir::new().expect("tempdir");
        // Treat the temp drive itself as retired; any resolved path lands on it, so even a path
        // outside the worktree must report RetiredDrive, not WorktreeEscape.
        let retired = format!("{}:", root_drive_letter(dir.path()));
        let other = TempDir::new().expect("tempdir2");
        let outside = other.path().join("x.txt");
        let resolved = resolve(&outside, dir.path(), &retired).expect("resolve");
        assert_eq!(resolved, Resolved::Rejected(RejectReason::RetiredDrive));
    }

    #[test]
    fn retired_drive_check_is_case_insensitive() {
        let dir = TempDir::new().expect("tempdir");
        let letter = root_drive_letter(dir.path());
        let retired = format!("{}:", letter.to_ascii_lowercase());
        let resolved = resolve(Path::new("inside.txt"), dir.path(), &retired).expect("resolve");
        assert_eq!(resolved, Resolved::Rejected(RejectReason::RetiredDrive));
    }

    #[test]
    fn permits_nonexistent_child_for_new_file_writes() {
        let dir = TempDir::new().expect("tempdir");
        // The file does not exist yet; its nearest existing ancestor is the worktree root.
        let resolved = resolve(Path::new("new/sub/file.txt"), dir.path(), "Q:").expect("resolve");
        match resolved {
            Resolved::Permitted(p) => assert!(is_contained(
                &p,
                &std::fs::canonicalize(dir.path()).expect("canon")
            )),
            other => panic!("expected Permitted, got {other:?}"),
        }
    }

    #[test]
    fn rejects_nonexistent_path_that_escapes_via_dotdot() {
        let dir = TempDir::new().expect("tempdir");
        // Non-existent tail with `..` that climbs out of the worktree.
        let resolved =
            resolve(Path::new(r"sub\..\..\escape.txt"), dir.path(), "Q:").expect("resolve");
        assert_eq!(resolved, Resolved::Rejected(RejectReason::WorktreeEscape));
    }

    #[test]
    fn sibling_prefix_is_not_contained() {
        // `C:\work-other` must not be considered inside `C:\work`.
        let root = Path::new(r"\\?\C:\work");
        let sibling = Path::new(r"\\?\C:\work-other\file.txt");
        assert!(!is_contained(sibling, root));
    }

    #[test]
    fn exact_root_is_contained() {
        let root = Path::new(r"\\?\C:\work");
        assert!(is_contained(root, root));
    }

    // Feature: builtin-tools, Property 1: no permitted path escapes confinement
    //
    // For any worktree root and any (including adversarial) candidate path, if `resolve`
    // returns `Permitted(p)` then `p` is component-wise contained within the canonicalized
    // worktree root and its drive letter is not the retired drive. We exercise `..` chains,
    // absolute paths pointing outside the root, and real symlink/junction targets built in
    // tempdirs that point outside the root. See design.md, Property 1
    // (Validates: Requirements 2.2, 2.7, 7.1, 7.2, 7.3, 7.4, 7.5, 7.7).
    mod property_no_escape {
        use super::*;
        use proptest::prelude::*;
        use std::os::windows::fs::{symlink_dir, symlink_file};
        use std::process::Command;

        /// A single lexical path segment that is safe to use as a file/dir name on Windows.
        fn segment() -> impl Strategy<Value = String> {
            "[a-z][a-z0-9_]{0,7}"
        }

        /// A relative candidate mixing normal segments with `..`/`.` so the generator covers
        /// deep escape chains as well as benign in-tree paths.
        fn relative_candidate() -> impl Strategy<Value = PathBuf> {
            prop::collection::vec(
                prop_oneof![
                    3 => segment(),
                    2 => Just("..".to_string()),
                    1 => Just(".".to_string()),
                ],
                0..8,
            )
            .prop_map(|parts| parts.iter().collect::<PathBuf>())
        }

        /// Which flavour of candidate to feed `resolve` for a given iteration.
        #[derive(Debug, Clone)]
        enum CandidateKind {
            /// A relative path (joined onto the worktree root inside `resolve`).
            Relative(PathBuf),
            /// An absolute path built inside a sibling tempdir, outside the worktree.
            AbsoluteOutside(Vec<String>),
            /// A symlink/junction placed in the worktree whose target lives outside it.
            LinkOutside { link_name: String, as_dir: bool },
        }

        fn candidate_kind() -> impl Strategy<Value = CandidateKind> {
            prop_oneof![
                relative_candidate().prop_map(CandidateKind::Relative),
                prop::collection::vec(segment(), 1..4).prop_map(CandidateKind::AbsoluteOutside),
                (segment(), any::<bool>()).prop_map(|(link_name, as_dir)| {
                    CandidateKind::LinkOutside { link_name, as_dir }
                }),
            ]
        }

        /// Pick a retired drive letter guaranteed to differ from the temp drive, so the
        /// retired-drive check never fires and we genuinely test worktree containment.
        fn non_temp_retired_drive(root: &Path) -> String {
            let temp_letter = drive_letter(&std::fs::canonicalize(root).expect("canonicalize"))
                .expect("drive letter")
                .to_ascii_uppercase();
            let alt = if temp_letter == 'Q' { 'Z' } else { 'Q' };
            format!("{alt}:")
        }

        /// Build the real on-disk candidate for a `CandidateKind`, returning the path to pass to
        /// `resolve`. For link cases, attempts to create a symlink then falls back to a junction
        /// (via `mklink /J`) when symlink privilege is unavailable; returns `None` if neither
        /// could be created so the iteration is skipped rather than falsely passing.
        fn materialize(kind: &CandidateKind, worktree: &Path, outside: &Path) -> Option<PathBuf> {
            match kind {
                CandidateKind::Relative(rel) => Some(rel.clone()),
                CandidateKind::AbsoluteOutside(parts) => {
                    let mut p = outside.to_path_buf();
                    for part in parts {
                        p.push(part);
                    }
                    Some(p)
                }
                CandidateKind::LinkOutside { link_name, as_dir } => {
                    let link = worktree.join(link_name);
                    if link.exists() {
                        return None; // name collision from a prior iteration path; skip
                    }
                    if *as_dir {
                        let target = outside.join("link_target_dir");
                        let _ = std::fs::create_dir_all(&target);
                        if symlink_dir(&target, &link).is_ok() {
                            return Some(link);
                        }
                        // Fall back to an NTFS junction, which needs no special privilege.
                        let ok = Command::new("cmd")
                            .args(["/C", "mklink", "/J"])
                            .arg(&link)
                            .arg(&target)
                            .output()
                            .map(|o| o.status.success())
                            .unwrap_or(false);
                        ok.then_some(link)
                    } else {
                        let target = outside.join("link_target_file.txt");
                        let _ = std::fs::write(&target, b"outside");
                        symlink_file(&target, &link).ok().map(|()| link)
                    }
                }
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn permitted_paths_never_escape_confinement(kind in candidate_kind()) {
                let worktree = TempDir::new().expect("worktree tempdir");
                let outside = TempDir::new().expect("outside tempdir");
                let retired = non_temp_retired_drive(worktree.path());

                let Some(candidate) = materialize(&kind, worktree.path(), outside.path()) else {
                    // Could not build this adversarial case on this host (e.g. no symlink
                    // privilege and junction creation failed); skip rather than pass falsely.
                    return Ok(());
                };

                let canonical_root =
                    std::fs::canonicalize(worktree.path()).expect("canonicalize root");

                let resolved = resolve(&candidate, worktree.path(), &retired)
                    .expect("resolve should not error on these inputs");

                if let Resolved::Permitted(p) = resolved {
                    // Core invariant: any permitted path is component-wise contained in the
                    // canonical worktree root...
                    prop_assert!(
                        is_contained(&p, &canonical_root),
                        "Permitted path {:?} escaped the worktree root {:?} (candidate kind: {:?})",
                        p,
                        canonical_root,
                        kind,
                    );
                    // ...and is not on the retired drive.
                    prop_assert!(
                        !on_retired_drive(&p, &retired),
                        "Permitted path {:?} is on the retired drive {} (candidate kind: {:?})",
                        p,
                        retired,
                        kind,
                    );
                }
            }
        }
    }

    // Feature: builtin-tools, Property 2: retired-drive rejection takes precedence
    //
    // Property 2: Retired-drive rejection takes precedence and carries a distinct reason.
    // Validates: Requirements 2.8, 2.9, 7.6.
    //
    // Technique: treat the temp worktree's OWN drive letter as the retired drive. Every candidate
    // we generate resolves onto that same drive, so each one must be rejected on the retired-drive
    // basis — even when the candidate also escapes the worktree (precedence, Req 2.9), when it is a
    // read-style target (Req 7.6 "including when the requested operation is a read"), and when it is
    // reached through a symlink/junction whose target lies on the retired drive (Req 2.8). The
    // reported reason must be `RetiredDrive`, distinct from `WorktreeEscape`, regardless of the
    // case or trailing `:`/`\` form of the configured retired-drive string (case-insensitive, 7.6).
    mod property_retired_drive_precedence {
        use super::*;
        use proptest::prelude::*;
        use std::os::windows::fs::symlink_dir;
        use std::process::Command;

        /// How the configured `retired_drive` string is spelled. The drive letter itself is always
        /// the worktree's own drive; only the surrounding form (case, suffix) varies, and all forms
        /// must compare equal case-insensitively (Requirement 7.6).
        #[derive(Clone, Copy, Debug)]
        enum DriveForm {
            Upper,           // "C"
            Lower,           // "c"
            UpperColon,      // "C:"
            LowerColon,      // "c:"
            UpperColonSlash, // "C:\"
            LowerColonSlash, // "c:\"
        }

        fn format_retired(letter: char, form: DriveForm) -> String {
            let upper = letter.to_ascii_uppercase();
            let lower = letter.to_ascii_lowercase();
            match form {
                DriveForm::Upper => upper.to_string(),
                DriveForm::Lower => lower.to_string(),
                DriveForm::UpperColon => format!("{upper}:"),
                DriveForm::LowerColon => format!("{lower}:"),
                DriveForm::UpperColonSlash => format!("{upper}:\\"),
                DriveForm::LowerColonSlash => format!("{lower}:\\"),
            }
        }

        fn drive_form_strategy() -> impl Strategy<Value = DriveForm> {
            prop_oneof![
                Just(DriveForm::Upper),
                Just(DriveForm::Lower),
                Just(DriveForm::UpperColon),
                Just(DriveForm::LowerColon),
                Just(DriveForm::UpperColonSlash),
                Just(DriveForm::LowerColonSlash),
            ]
        }

        /// The flavour of candidate to resolve. Each lands on the worktree's drive one way or
        /// another, so each must be rejected on the retired-drive basis.
        #[derive(Clone, Debug)]
        enum Candidate {
            /// A relative path inside the worktree (a plain read/write target on the retired drive).
            InsideRelative(Vec<String>),
            /// A relative `..` chain that escapes the worktree — also outside, so precedence applies.
            DotDotEscape(usize, String),
            /// An absolute path on the same drive but outside the worktree root.
            AbsoluteOutside(String),
            /// A path reached through a directory junction/symlink whose target is outside the root.
            JunctionTarget(String),
        }

        /// Lowercase ASCII path segments, safe on NTFS and never empty.
        fn segment() -> impl Strategy<Value = String> {
            "[a-z][a-z0-9_]{0,7}".prop_map(|s| s)
        }

        fn candidate_strategy() -> impl Strategy<Value = Candidate> {
            prop_oneof![
                prop::collection::vec(segment(), 1..4).prop_map(Candidate::InsideRelative),
                (1usize..5, segment()).prop_map(|(n, s)| Candidate::DotDotEscape(n, s)),
                segment().prop_map(Candidate::AbsoluteOutside),
                segment().prop_map(Candidate::JunctionTarget),
            ]
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(200))]

            #[test]
            fn retired_drive_rejection_takes_precedence_and_is_distinct(
                candidate in candidate_strategy(),
                form in drive_form_strategy(),
            ) {
                // A fresh worktree per case. `outside` is a sibling tempdir on the same drive used
                // as the escape target for the absolute-outside and junction candidates.
                let worktree = TempDir::new().expect("worktree tempdir");
                let outside = TempDir::new().expect("outside tempdir");

                // The worktree's own drive is treated as retired, so EVERYTHING on it is rejected.
                let letter = root_drive_letter(worktree.path());
                let retired = format_retired(letter, form);

                let resolved = match &candidate {
                    Candidate::InsideRelative(parts) => {
                        let rel: PathBuf = parts.iter().collect();
                        resolve(&rel, worktree.path(), &retired)
                    }
                    Candidate::DotDotEscape(depth, leaf) => {
                        let mut rel = PathBuf::new();
                        for _ in 0..*depth {
                            rel.push("..");
                        }
                        rel.push(leaf);
                        resolve(&rel, worktree.path(), &retired)
                    }
                    Candidate::AbsoluteOutside(leaf) => {
                        let abs = outside.path().join(leaf);
                        resolve(&abs, worktree.path(), &retired)
                    }
                    Candidate::JunctionTarget(leaf) => {
                        // Create a directory link inside the worktree pointing at the outside
                        // tempdir, then resolve a path through it. The resolver must follow the link
                        // (Req 7.2) and reject the target on the retired drive (Req 2.8, 7.6). Try a
                        // directory symlink first; fall back to an NTFS junction (`mklink /J`, needs
                        // no privilege); if neither can be created, resolve the plain outside path
                        // so the case still exercises precedence rather than falsely passing.
                        let link = worktree.path().join("link_dir");
                        let via_link = if symlink_dir(outside.path(), &link).is_ok() {
                            true
                        } else {
                            Command::new("cmd")
                                .args(["/C", "mklink", "/J"])
                                .arg(&link)
                                .arg(outside.path())
                                .output()
                                .map(|o| o.status.success())
                                .unwrap_or(false)
                        };
                        if via_link {
                            let through = Path::new("link_dir").join(leaf);
                            resolve(&through, worktree.path(), &retired)
                        } else {
                            let abs = outside.path().join(leaf);
                            resolve(&abs, worktree.path(), &retired)
                        }
                    }
                };

                let resolved = resolved.expect("resolve should not error for these candidates");

                // Core assertions: every candidate lands on the retired drive, so the reason must be
                // RetiredDrive (Req 7.6) — taking precedence over any worktree escape (Req 2.9) —
                // and it must be the distinct retired-drive value, never WorktreeEscape.
                prop_assert_eq!(
                    &resolved,
                    &Resolved::Rejected(RejectReason::RetiredDrive),
                    "candidate {:?} with retired form {:?} must be rejected on the retired-drive basis",
                    candidate,
                    form,
                );
                prop_assert_ne!(resolved, Resolved::Rejected(RejectReason::WorktreeEscape));
            }
        }
    }
}
