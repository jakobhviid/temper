//! The provider interface, as data.
//!
//! ARCHITECTURE describes eleven columns every provider must answer. Prose
//! cannot fail a build, so the table lives here and the tests below hold it to
//! the finding registry in `plan::KIND_ANSWERS`.
//!
//! What this catches is the defect that keeps recurring at the *feature* level
//! rather than the finding level: a provider that reports a direction it has no
//! way to act on, or claims a verb that has no code path for it. Registering
//! `temper reconcile` as rpm-ostree's absorb answer before writing that path
//! re-created exactly that bug for the length of one commit — the finding
//! registry made it loud, but nothing related the *provider* to the verbs.
//!
//! This is the registry half of the interface, not the dispatch half. Providers
//! still have their own function signatures; harmonising those behind a real
//! trait is the remaining work, and it is deliberately sequenced after enough
//! providers actually fill their columns to shape it.

/// How a column is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Col {
    /// Implemented.
    Yes,
    /// Not implemented, and why. A reason is mandatory: the awkwardness is
    /// where you ask whether the thing simply has not been built.
    No(&'static str),
    /// The column does not apply to this kind of state, and why.
    NA(&'static str),
}

impl Col {
    pub fn answered(&self) -> bool {
        matches!(self, Col::Yes)
    }
}

/// One provider's answers. Field order matches ARCHITECTURE's column order.
#[derive(Debug, Clone, Copy)]
pub struct ProviderSpec {
    pub name: &'static str,
    /// The `Finding.kind`s this provider emits — every one must be registered
    /// in `plan::KIND_ANSWERS`, and no kind may belong to two providers.
    pub kinds: &'static [&'static str],
    /// 1: a group declaration exists.
    pub fleet_scope: Col,
    /// 2: a machine declaration exists. Without it the provider has no spec
    /// column and cannot be reconciled.
    pub machine_scope: Col,
    /// 3: can enumerate what is present, distinguishing "none" from "cannot ask".
    pub observe: Col,
    /// 4: can make the machine match.
    pub install: Col,
    /// 5: can remove what neither scope declares.
    pub prune: Col,
    /// 6/7: reconcile absorbs into, and drops from, machine scope.
    pub reconcile: Col,
    /// 8: an ignore list, at machine scope.
    pub ignore: Col,
    /// 10: mutations are journaled and revertible.
    pub revertible: Col,
    /// 11: what happens to what it deployed when the declaration goes away.
    pub residue: Col,
}

const NOT_JOURNALED: &str =
    "installs are not journaled for this provider yet — the pattern applies (the \
     missing set is known before the converge, and uninstall is install backwards), \
     it is simply unwired";
const NO_RESIDUE: &str =
    "a package leaves no spec-owned residue: removing the declaration makes it an \
     extra, which prune answers";

/// **VS Code extensions are deliberately not a provider here.** temper parses a
/// `vscode "…"` token and will converge one if you declare it, but the probe
/// invariant means a spec that declares none never runs `code --list-extensions`
/// — so VS Code Settings Sync stays the sole registrar of your extensions and
/// nothing is ever reported as an extra. Listing it as a managed provider would
/// claim an ownership temper does not want and Settings Sync already has.
pub const PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        name: "brew",
        kinds: &["package", "package-extra"],
        fleet_scope: Col::Yes,
        machine_scope: Col::Yes,
        observe: Col::Yes,
        install: Col::Yes,
        // The one real deviation from install/uninstall symmetry, and it is a
        // correctness requirement: `brew bundle cleanup` is dependency-aware, so
        // a formula kept only as another package's transitive dependency is not
        // removed. `brew uninstall` would take it.
        prune: Col::Yes,
        reconcile: Col::Yes,
        ignore: Col::Yes,
        revertible: Col::Yes,
        residue: Col::NA(NO_RESIDUE),
    },
    ProviderSpec {
        name: "brew-trust",
        kinds: &["brew-trust", "brew-trust-extra"],
        fleet_scope: Col::Yes,
        machine_scope: Col::Yes,
        observe: Col::Yes,
        install: Col::Yes,
        prune: Col::Yes,
        reconcile: Col::Yes,
        ignore: Col::Yes,
        revertible: Col::No(NOT_JOURNALED),
        residue: Col::NA("trust is a flag, not a deployed artifact"),
    },
    ProviderSpec {
        name: "flatpak",
        kinds: &["package", "package-extra"],
        fleet_scope: Col::Yes,
        machine_scope: Col::Yes,
        observe: Col::Yes,
        install: Col::Yes,
        prune: Col::Yes,
        reconcile: Col::Yes,
        ignore: Col::Yes,
        revertible: Col::Yes,
        residue: Col::NA(NO_RESIDUE),
    },
    ProviderSpec {
        name: "mas",
        kinds: &["package", "package-extra"],
        fleet_scope: Col::Yes,
        machine_scope: Col::Yes,
        observe: Col::Yes,
        // Forgiving on purpose: a MAS failure (no App Store sign-in, an app not
        // tied to this Apple ID) is warned and skipped, never fatal.
        install: Col::Yes,
        prune: Col::Yes,
        reconcile: Col::Yes,
        ignore: Col::Yes,
        // `mas uninstall (<id>…|--all)` takes ids — which is what `match_name`
        // yields for this manager — and requires root, so undo prompts.
        revertible: Col::Yes,
        residue: Col::NA(NO_RESIDUE),
    },
    ProviderSpec {
        name: "gnome-extensions",
        kinds: &["gnome-extension", "gnome-extension-extra"],
        fleet_scope: Col::Yes,
        machine_scope: Col::Yes,
        observe: Col::Yes,
        install: Col::Yes,
        prune: Col::Yes,
        reconcile: Col::Yes,
        ignore: Col::Yes,
        revertible: Col::Yes,
        residue: Col::NA(NO_RESIDUE),
    },
    ProviderSpec {
        name: "rpm-ostree",
        kinds: &["rpm-ostree", "rpm-ostree-extra"],
        fleet_scope: Col::Yes,
        machine_scope: Col::Yes,
        observe: Col::Yes,
        install: Col::Yes,
        prune: Col::Yes,
        reconcile: Col::Yes,
        ignore: Col::Yes,
        revertible: Col::Yes,
        residue: Col::NA(NO_RESIDUE),
    },
    ProviderSpec {
        name: "dconf",
        kinds: &["dconf-key", "dconf-extra", "dconf-uncaptured", "dconf-unavailable"],
        // `setkey` steps in a bundle are the fleet-scope declaration; a
        // `[[machine.dconf]]` snapshot is the machine-scope one.
        fleet_scope: Col::Yes,
        machine_scope: Col::Yes,
        observe: Col::Yes,
        install: Col::Yes,
        prune: Col::NA(
            "a snapshot is not exhaustive — nothing deletes a live key it never mentioned",
        ),
        reconcile: Col::Yes,
        // `strip` is a noise filter, not an ignore list: it drops keys that
        // would corrupt a capture/restore round trip, not keys you chose to
        // stop tracking.
        ignore: Col::No("no per-key ignore list; `strip` is a round-trip noise filter"),
        revertible: Col::Yes,
        residue: Col::No(
            "removing a [[machine.dconf]] leaves its captured file and its live keys — \
             nothing tracks what a snapshot owned",
        ),
    },
];

pub fn spec(name: &str) -> Option<&'static ProviderSpec> {
    PROVIDERS.iter().find(|p| p.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{kind_spec, Answer};

    /// Every kind a provider claims is registered, with both directions.
    #[test]
    fn provider_kinds_are_registered_findings() {
        for p in PROVIDERS {
            assert!(!p.kinds.is_empty(), "provider `{}` emits no kinds", p.name);
            for k in p.kinds {
                assert!(
                    kind_spec(k).is_some(),
                    "provider `{}` emits kind `{k}`, which plan::KIND_ANSWERS does not register",
                    p.name
                );
            }
        }
    }

    /// A provider that says it can prune must have a kind whose converge answer
    /// actually names `temper prune`, and vice versa.
    ///
    /// This is the feature-level version of "advice must be executable". A
    /// column answered `Yes` with no verb behind it is the same lie as a
    /// remediation naming a verb with no code path — just one level up, where
    /// nothing was checking.
    #[test]
    fn a_claimed_prune_is_a_prune_someone_can_run() {
        for p in PROVIDERS {
            let names_prune = p.kinds.iter().filter_map(|k| kind_spec(k)).any(|k| {
                k.converge
                    .iter()
                    .chain(k.absorb)
                    .any(|a| matches!(a, Answer::Verb(c) if *c == "temper prune"))
            });
            assert_eq!(
                p.prune.answered(),
                names_prune,
                "provider `{}` claims prune={:?} but its registered kinds {} name `temper prune`",
                p.name,
                p.prune,
                if names_prune { "do" } else { "do not" }
            );
        }
    }

    /// Same contract for reconcile: claiming the spec column means a kind of
    /// this provider actually offers `temper reconcile`.
    #[test]
    fn a_claimed_reconcile_is_a_reconcile_someone_can_run() {
        for p in PROVIDERS {
            let names_reconcile = p.kinds.iter().filter_map(|k| kind_spec(k)).any(|k| {
                k.absorb
                    .iter()
                    .any(|a| matches!(a, Answer::Verb(c) if *c == "temper reconcile"))
            });
            assert_eq!(
                p.reconcile.answered(),
                names_reconcile,
                "provider `{}` claims reconcile={:?}, but its kinds disagree",
                p.name,
                p.reconcile
            );
        }
    }

    /// Machine scope is what makes a provider reconcilable at all, so claiming
    /// reconcile without it is incoherent (Principle #12).
    #[test]
    fn reconcile_requires_a_machine_scope_to_write_to() {
        for p in PROVIDERS {
            if p.reconcile.answered() {
                assert!(
                    p.machine_scope.answered(),
                    "provider `{}` claims reconcile but declares no machine scope — \
                     there is nowhere for an absorb to land",
                    p.name
                );
            }
        }
    }

    /// A declined column carries a real reason. `No("")` is how a gap becomes
    /// invisible again.
    #[test]
    fn declining_a_column_requires_saying_why() {
        for p in PROVIDERS {
            for (col, label) in [
                (p.fleet_scope, "fleet_scope"),
                (p.machine_scope, "machine_scope"),
                (p.observe, "observe"),
                (p.install, "install"),
                (p.prune, "prune"),
                (p.reconcile, "reconcile"),
                (p.ignore, "ignore"),
                (p.revertible, "revertible"),
                (p.residue, "residue"),
            ] {
                if let Col::No(why) | Col::NA(why) = col {
                    assert!(
                        why.len() > 20,
                        "provider `{}` declines `{label}` without a real reason",
                        p.name
                    );
                }
            }
        }
    }
}
