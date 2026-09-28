//! The three orthogonal axes of a mod's presence in a pack (side+required
//! rework): which side it executes on, whether joining a server requires both
//! sides to carry it, and the launcher-facing presence class computed from the
//! two plus the dependency graph. Pure vocab -- no I/O.
//!
//! "Unknown" is deliberately not a variant anywhere: an undecided axis is an
//! `Option::None`, which never serializes outward. The classifier (stage D/E)
//! is responsible for shrinking `None` to a value or reporting the mod
//! `unclassified`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

/// Axis A -- which side a mod's code executes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
#[serde(rename_all = "snake_case")]
pub enum SideClass {
    Client,
    Server,
    Both,
}

/// Axis B -- whether a mod must be present on both sides for a client to join
/// a server carrying it. Orthogonal to [`SideClass`]: a Both-side mod with
/// `acceptableRemoteVersions = "*"` / `displayTest = NONE` runs everywhere but
/// tolerates the other side lacking it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
#[serde(rename_all = "snake_case")]
pub enum MatchPolicy {
    MustMatch,
    Tolerant,
}

/// The launcher-facing presence class of one mod in one pack -- the computed
/// output of the two axes plus the dependency graph. `Required` means the
/// graph locked the mod, and never names a client-side one (the client-mod
/// invariant); a server-side mod reads required when an enabled mod cannot
/// start without it. The optional classes carry the side so a launcher can
/// badge the toggle;
/// `Coremod` marks a jar that is not a mod at all (a bare ASM/loader plugin),
/// which is always toggleable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
#[serde(rename_all = "snake_case")]
pub enum PresenceClass {
    Required,
    OptionalClient,
    OptionalServer,
    OptionalBoth,
    Coremod,
}

impl SideClass {
    pub fn as_str(self) -> &'static str {
        match self {
            SideClass::Client => "client",
            SideClass::Server => "server",
            SideClass::Both => "both",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "client" => SideClass::Client,
            "server" => SideClass::Server,
            "both" => SideClass::Both,
            _ => return None,
        })
    }
}

impl MatchPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            MatchPolicy::MustMatch => "must_match",
            MatchPolicy::Tolerant => "tolerant",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "must_match" => MatchPolicy::MustMatch,
            "tolerant" => MatchPolicy::Tolerant,
            _ => return None,
        })
    }
}

impl PresenceClass {
    pub fn as_str(self) -> &'static str {
        match self {
            PresenceClass::Required => "required",
            PresenceClass::OptionalClient => "optional_client",
            PresenceClass::OptionalServer => "optional_server",
            PresenceClass::OptionalBoth => "optional_both",
            PresenceClass::Coremod => "coremod",
        }
    }
}

/// Map a Modrinth project's declared environment flags (`client_side` /
/// `server_side`, each `required | optional | unsupported`) onto the two axes.
///
/// `must_match` is exactly (client required AND server required); a side is
/// `Client`/`Server` when the other side is `unsupported`; every other
/// supported combination is a tolerant Both. `unsupported`x`unsupported` and
/// any unrecognized value (Modrinth also ships literal `"unknown"`) yield
/// `None` -- the flags decide nothing and the bytecode classifier stays the
/// arbiter for that mod.
pub fn side_from_modrinth_env(client: &str, server: &str) -> Option<(SideClass, MatchPolicy)> {
    let known = |v: &str| matches!(v, "required" | "optional" | "unsupported");
    if !known(client) || !known(server) {
        return None;
    }
    let side = match (client == "unsupported", server == "unsupported") {
        (true, true) => return None,
        (true, false) => SideClass::Server,
        (false, true) => SideClass::Client,
        (false, false) => SideClass::Both,
    };
    let policy = if client == "required" && server == "required" {
        MatchPolicy::MustMatch
    } else {
        MatchPolicy::Tolerant
    };
    Some((side, policy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modrinth_env_maps_all_nine_combinations() {
        use MatchPolicy::*;
        use SideClass::*;
        let cases = [
            ("required", "required", Some((Both, MustMatch))),
            ("required", "optional", Some((Both, Tolerant))),
            ("required", "unsupported", Some((Client, Tolerant))),
            ("optional", "required", Some((Both, Tolerant))),
            ("optional", "optional", Some((Both, Tolerant))),
            ("optional", "unsupported", Some((Client, Tolerant))),
            ("unsupported", "required", Some((Server, Tolerant))),
            ("unsupported", "optional", Some((Server, Tolerant))),
            ("unsupported", "unsupported", None),
        ];
        for (c, s, want) in cases {
            assert_eq!(side_from_modrinth_env(c, s), want, "({c}, {s})");
        }
    }

    #[test]
    fn modrinth_env_unknown_value_decides_nothing() {
        assert_eq!(side_from_modrinth_env("unknown", "required"), None);
        assert_eq!(side_from_modrinth_env("required", "unknown"), None);
        assert_eq!(side_from_modrinth_env("", ""), None);
    }

    #[test]
    fn vocab_round_trips_through_strings() {
        for side in [SideClass::Client, SideClass::Server, SideClass::Both] {
            assert_eq!(SideClass::parse(side.as_str()), Some(side));
        }
        for p in [MatchPolicy::MustMatch, MatchPolicy::Tolerant] {
            assert_eq!(MatchPolicy::parse(p.as_str()), Some(p));
        }
        assert_eq!(SideClass::parse("either"), None);
        assert_eq!(MatchPolicy::parse(""), None);
    }

    #[test]
    fn presence_serializes_as_snake_case_strings() {
        let json = serde_json::to_string(&PresenceClass::OptionalClient).unwrap();
        assert_eq!(json, "\"optional_client\"");
        let back: PresenceClass = serde_json::from_str("\"coremod\"").unwrap();
        assert_eq!(back, PresenceClass::Coremod);
    }
}
