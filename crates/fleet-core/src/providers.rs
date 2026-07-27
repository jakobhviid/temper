//! Package managers, modeled uniformly as (converge, probe) at machine scope.
//!
//! - brew       — one aggregate `brew bundle`; internalizes tap-trust; knows the
//!                `vscode` sub-type. Drift is dependency-aware (`brew bundle
//!                cleanup`), which is why packages must converge whole, not
//!                per-app.
//! - flatpak    — aggregate set with an ignore-list; `flatpak override` handled
//!                as a setkey-style op.
//! - mas        — forgiving: reports & skips on missing sign-in, never fatal.
//! - gext       — GNOME extensions (install from EGO + update); distinct from
//!                *enabling* them (a dconf key).
//! - rpm-ostree — layer an rpm that can't be image-baked; emits a
//!                reboot-required signal fleet reports but never automates.
//!
//! probe vocabulary: binary | brew | cask | flatpak | mas | gext | rpm | path
//! | exec — used to gate app-scope config on reality.

// TODO: trait Provider { fn converge(effective_set); fn probe(id) -> bool;
//       fn extras(declared) -> Vec<Pkg>; } + the cask reset-before-converge hook.
