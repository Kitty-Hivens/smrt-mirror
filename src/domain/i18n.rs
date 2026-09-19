//! Text a curator writes more than once, once per language.
//!
//! Two things carry it: a build's release notes and a pack's card. Both keep the
//! untagged field they always had and put a map beside it keyed by language tag,
//! and both have to settle it the same way, or a client reads one rule for the
//! notes and another for the card it renders three lines above them.

use std::collections::BTreeMap;

/// The map as it ships: one entry per language, blanks left out.
///
/// Tags are trimmed and lower-cased, so `EN`, ` en ` and `en` are one entry
/// rather than three a reader has to reconcile. A translation left empty is
/// absent rather than published as nothing, which is what makes a missing key
/// mean "not written" and never "written as nothing". An empty map is `None`,
/// so it stays off the wire entirely.
pub fn settle(map: Option<BTreeMap<String, String>>) -> Option<BTreeMap<String, String>> {
    map.map(|m| {
        m.into_iter()
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .filter(|(k, v)| !k.is_empty() && !v.is_empty())
            .collect::<BTreeMap<_, _>>()
    })
    .filter(|m| !m.is_empty())
}

/// What the untagged field falls back to when the curator wrote only
/// translations: the language this deployment's audience reads
/// (`SMRT_DEFAULT_LANGUAGE`, `en` unless set), then whichever language sorts
/// first.
///
/// Some text beats none. The untagged field is the one every client has always
/// read, so leaving it empty while the map is full would hide the card, or the
/// release note, from everything that has not learned about the map yet.
///
/// Which translation fills it is not a detail. Today it is what every player
/// sees, the launcher reading no map at all, so a mirror serving a Russian
/// community and filling from English hands its players text they cannot read.
/// The preference is the operator's because the audience is.
pub fn untagged(map: &Option<BTreeMap<String, String>>, prefer: &str) -> Option<String> {
    let prefer = prefer.trim().to_ascii_lowercase();
    map.as_ref()
        .and_then(|m| m.get(&prefer).or_else(|| m.values().next()))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> Option<BTreeMap<String, String>> {
        Some(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn a_language_left_empty_is_absent_rather_than_empty() {
        let settled = settle(map(&[("en", "Heavy industry."), ("ru", "   ")])).expect("english");
        assert_eq!(settled.keys().collect::<Vec<_>>(), ["en"]);
    }

    #[test]
    fn one_language_is_one_entry_however_its_tag_is_written() {
        let settled = settle(map(&[(" EN ", "Heavy industry."), ("ru", "Тяжпром.")])).unwrap();
        assert_eq!(settled.keys().collect::<Vec<_>>(), ["en", "ru"]);
    }

    #[test]
    fn nothing_written_stays_nothing() {
        assert_eq!(settle(None), None);
        assert_eq!(settle(map(&[("ru", ""), ("", "orphan")])), None);
    }

    #[test]
    fn the_untagged_copy_takes_the_language_the_deployment_reads() {
        let both = map(&[("ru", "Тяжпром."), ("en", "Heavy industry.")]);
        assert_eq!(untagged(&both, "en").as_deref(), Some("Heavy industry."));
        // the case this exists for: the mirror's own audience reads Russian, so
        // the copy every untranslated client gets is the Russian one
        assert_eq!(untagged(&both, "ru").as_deref(), Some("Тяжпром."));
        assert_eq!(untagged(&both, " RU ").as_deref(), Some("Тяжпром."));
    }

    #[test]
    fn a_language_nobody_wrote_still_yields_what_there_is() {
        // some text beats none, whatever the deployment prefers
        assert_eq!(
            untagged(&map(&[("ru", "Тяжпром.")]), "de").as_deref(),
            Some("Тяжпром.")
        );
        assert_eq!(untagged(&None, "ru"), None);
    }
}
