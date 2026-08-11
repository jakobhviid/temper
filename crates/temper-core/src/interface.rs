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

/// Why this provider's converge cannot be reverted, if it cannot.
///
/// Read at runtime, so the table is a source rather than a comment: the same
/// knowledge was hand-written in `plan` beside it, which is how a table and the
/// code it describes drift apart. A provider whose `revertible` flips to `Yes`
/// stops being reported here without anyone remembering to look.
pub fn unrevertible_reason(name: &str) -> Option<&'static str> {
    match spec(name)?.revertible {
        Col::No(why) => Some(why),
        _ => None,
    }
}

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
        kinds: &["brew-package", "brew-package-extra"],
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
        kinds: &["flatpak-package", "flatpak-package-extra"],
        fleet_scope: Col::Yes,
        machine_scope: Col::Yes,
        observe: Col::Yes,
        install: Col::Yes,
        // Removes user-scope apps and *reports* the system-scope ones it
        // declined, which is a real path with an honest answer — so `Yes`.
        prune: Col::Yes,
        reconcile: Col::Yes,
        ignore: Col::Yes,
        // Not the same claim. `install` runs `flatpak install` with no scope
        // flag, whose default is the SYSTEM installation, while undo uninstalls
        // `--user` — so wherever the apps live system-wide (flatpak's default,
        // and every storefront's) a revert finds nothing and reports success.
        // That is a bug, not a design question: the bar is the storefront the
        // desktop already ships, which removes a system app with no privilege
        // at all, so temper owns the installation its converge writes to.
        // See ROADMAP, "Bugs".
        revertible: Col::No(
            "undo uninstalls `--user`, which is not necessarily the scope install \
             wrote to — see ROADMAP",
        ),
        residue: Col::NA(NO_RESIDUE),
    },
    ProviderSpec {
        name: "flatpak-remote",
        kinds: &["flatpak-remote", "flatpak-remote-extra"],
        // No fleet list: a remote belongs with the bundle whose apps need it,
        // which is the group scope and is gated. A fleet-wide remote nobody can
        // gate is the shape that made `[brew].trust` a problem.
        fleet_scope: Col::NA(
            "a remote belongs to the bundle whose apps need it — group scope, gated",
        ),
        machine_scope: Col::Yes,
        observe: Col::Yes,
        install: Col::Yes,
        prune: Col::Yes,
        reconcile: Col::Yes,
        ignore: Col::Yes,
        revertible: Col::No(
            "remote-add is not journaled yet; the pattern applies but is unwired",
        ),
        residue: Col::NA(
            "a remote leaves nothing behind: un-declaring it makes it an extra, which prune answers",
        ),
    },
    ProviderSpec {
        name: "mas",
        kinds: &["mas-package", "mas-package-extra"],
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
        name: "deployed-files",
        kinds: &["deployed-file-extra"],
        fleet_scope: Col::Yes,
        machine_scope: Col::NA(
            "a deployed file is declared by a step in a bundle; there is no per-machine file list",
        ),
        // The ledger is what makes this observable at all: a filesystem cannot
        // be asked which of its files temper wrote.
        observe: Col::Yes,
        install: Col::Yes,
        prune: Col::Yes,
        reconcile: Col::NA(
            "re-declaring a dropped file means writing the step you deleted — authoring, not reconciling",
        ),
        ignore: Col::No(
            "no ignore list for deployed paths yet; an edited file is reported rather than removed, \
             which covers the case that matters",
        ),
        revertible: Col::Yes,
        residue: Col::Yes,
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
    ProviderSpec {
        name: "profile",
        kinds: &["profile"],
        // The weakest row in the matrix, scored honestly. It had no entry at
        // all, so ARCHITECTURE could say whatever it liked about it and nothing
        // checked: `every_provider_is_in_the_architecture_matrix` runs code→doc
        // only, and a doc row with no provider behind it is invisible to it.
        fleet_scope: Col::Yes,
        machine_scope: Col::No(
            "a .mobileconfig is declared in a bundle; there is no per-machine \
             profile list, so nothing can be reconciled into one",
        ),
        // Reading what is installed needs neither MDM nor root, so this half is
        // real: `system_profiler` across the user and device scopes.
        observe: Col::Yes,
        install: Col::No(
            "apply is a System Settings dialog the user approves, so it cannot \
             converge unattended or headless",
        ),
        prune: Col::No("nothing removes an installed profile — that is a GUI action too"),
        reconcile: Col::No("no machine scope to absorb into, and no export of an installed profile"),
        ignore: Col::No("no extras direction exists, so there is nothing to silence"),
        revertible: Col::No("approval is the user's click; temper cannot un-approve it"),
        residue: Col::No(
            "an installed profile outlives the declaration and nothing enumerates \
             what temper put there — the file ledger covers deployed files, not \
             system profiles",
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

    /// The table is READ at runtime, not merely asserted about.
    ///
    /// `interface::PROVIDERS` was, for a while, referenced by no production code
    /// at all: a capability table checked only by its own tests is a comment
    /// with a test suite. `plan` derives the "what this run cannot take back"
    /// list from it, so a provider that flips to `revertible: Yes` stops being
    /// reported without anyone remembering to look.
    #[test]
    fn the_revertible_column_is_the_source_for_the_report() {
        // The three the package phase touches without journaling.
        for name in ["brew-trust", "flatpak-remote", "flatpak"] {
            let why = super::unrevertible_reason(name)
                .unwrap_or_else(|| panic!("`{name}` should declare why it is not revertible"));
            assert!(
                why.len() > 20,
                "`{name}` needs a real reason, got {why:?}"
            );
        }
        // …and one that IS revertible answers None, so the report stays quiet.
        assert!(super::unrevertible_reason("brew").is_none());
        assert!(super::unrevertible_reason("not-a-provider").is_none());
    }

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

    /// A matrix row must have a provider behind it — the doc→code direction.
    ///
    /// Its counterpart below runs code→doc. Only one of the two existed for a
    /// long time, which is how `profile` sat in the table with nothing asserting
    /// any of its cells. A doc row is a claim; a claim wants an owner.
    #[test]
    fn every_matrix_row_has_a_provider() {
        let doc = include_str!("../../../ARCHITECTURE.md");
        let start = doc.find("### Where each feature stands").expect("matrix section");
        for line in doc[start..].lines().skip_while(|l| !l.starts_with("| `")) {
            if !line.starts_with("| `") {
                break; // end of the table
            }
            let name = line
                .trim_start_matches("| `")
                .split('`')
                .next()
                .unwrap_or_default();
            // Rows that name several providers at once (`brew` / `cask` / …) are
            // keyed by the first, which is the one with the spec.
            assert!(
                spec(name).is_some(),
                "ARCHITECTURE's matrix has a row for `{name}` that no ProviderSpec \
                 answers for — nothing checks any of its eleven cells"
            );
        }
    }

    /// Every provider appears in the ARCHITECTURE matrix — the code→doc
    /// direction, and the counterpart to the check above.
    ///
    /// The matrix is how a reader learns where a feature stands, and a provider
    /// missing from it reads as one that does not exist. Docs are compiled into
    /// `--llm`, so a stale matrix does not merely read wrong — it misleads every
    /// agent that builds a spec from it (AGENTS.md).
    #[test]
    fn every_provider_is_in_the_architecture_matrix() {
        let doc = include_str!("../../../ARCHITECTURE.md");
        let matrix = {
            let start = doc.find("### Where each feature stands").expect("matrix section");
            &doc[start..]
        };
        for p in PROVIDERS {
            assert!(
                matrix.contains(&format!("`{}`", p.name)),
                "provider `{}` is not in the ARCHITECTURE feature matrix — a provider \
                 missing from it reads as one that does not exist",
                p.name
            );
        }
    }

    /// No kind belongs to two providers.
    ///
    /// The doc comment on `ProviderSpec.kinds` has always said this; nothing
    /// checked it, and brew, flatpak and mas all declared `package` /
    /// `package-extra`. Two consequences, both bad. A per-provider capability
    /// answer was inexpressible — every provider inherited every other's, so
    /// giving flatpak an honest one meant contradicting brew's. And the
    /// cross-check became satisfiable by declaration alone: a `ProviderSpec` for
    /// `apt` with those kinds and every column `Yes` passed the whole suite with
    /// no apt code anywhere, because *brew's* kind named the verbs.
    #[test]
    fn a_kind_belongs_to_exactly_one_provider() {
        let mut seen: std::collections::BTreeMap<&str, &str> = Default::default();
        for p in PROVIDERS {
            for k in p.kinds {
                if let Some(other) = seen.insert(k, p.name) {
                    panic!(
                        "kind `{k}` is claimed by both `{other}` and `{}` — a shared \
                         kind means neither can be answered for on its own",
                        p.name
                    );
                }
            }
        }
    }

    /// The matrix's `revertible` cell says what the table says.
    ///
    /// The doc scored `flatpak` as ✅ revertible while the code had just been
    /// changed to No — twice in one session, in both directions. A row that
    /// contradicts the table is worse than no row: ROADMAP rides `--llm`
    /// precisely so an agent can tell a working cell from a broken one.
    ///
    /// Only this column is checked, because only this one has an unambiguous
    /// glyph mapping (`Yes` → ✅, anything else → ❌/⚠). Checking all eleven
    /// would mean parsing the whole table, which rots faster than it catches.
    #[test]
    fn the_matrix_agrees_with_the_table_about_revertibility() {
        let doc = include_str!("../../../ARCHITECTURE.md");
        let start = doc.find("### Where each feature stands").expect("matrix section");
        for p in PROVIDERS {
            // The row is the matrix line naming this provider first.
            let Some(row) = doc[start..]
                .lines()
                .find(|l| l.starts_with(&format!("| `{}`", p.name)))
            else {
                continue; // covered by every_provider_is_in_the_architecture_matrix
            };
            let cells: Vec<&str> = row.split('|').map(str::trim).collect();
            // ["", name, 1 fleet, 2 machine, 3 obs, 4 inst, 5 prune, 6 r+, 7 r−,
            //  8 ign, 9 drift, 10 rev, 11 res, ""] → `revertible` is index 11.
            let Some(cell) = cells.get(11) else { continue };
            let doc_says_yes = *cell == "✅";
            assert_eq!(
                doc_says_yes,
                p.revertible.answered(),
                "ARCHITECTURE scores `{}` revertible as {cell:?} but the table says                  {:?} — one of them is lying to whoever reads `--llm`",
                p.name,
                p.revertible
            );
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
