//! **In what order a profile's libraries are shown — the rules, pure.**
//!
//! [`Preferences::library_order`](super::session::Preferences::library_order) is the record (on disk,
//! keyed by profile, naming libraries by `(machineIdentifier, section key)`); `browse`'s section table
//! is the live set of libraries. This module is the policy between them and holds neither, the way
//! [`pins`](super::pins) does for Favorite libraries, so each rule below is graded by `cargo test
//! --lib` rather than observed on a television.
//!
//! * **The default is the server order**: the caller hands the libraries down already in it (own
//!   servers first, then shares, each server's Directory order). An absent record is that order.
//! * **A recorded order is a projection, never a mutation.** [`effective`] answers a permutation of
//!   the libraries it was handed; no table index moves, so every index that names a library
//!   (`cur`, a saved view, a listing) stays valid.
//! * **A library the record does not name** (one created since, a share that arrived later, a
//!   library whose server has not told us its machine id) falls at its default position relative to
//!   the recorded ones: right after the library that precedes it in the default order.
//! * **A recorded library that is not here** (its server is absent for now, or it was deleted) costs
//!   nothing and keeps its record, so it returns to its place with its server.
//! * **Only a real reorder is recorded**: [`is_server_order`] says whether an order is the default,
//!   and the writer deletes the record instead of storing one that says nothing.
//!
//! [`hub_library`] is the other half of Home's use of this: which library a `/hubs` shelf belongs to,
//! from its `hubIdentifier` alone, so the answer does not depend on whether the shelf still holds
//! its cards.
use super::session::LibraryOrder;

/// One library as these rules see it: the join key a record names it by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lib<'a> {
    /// the server's `machineIdentifier`; empty while nobody has learned it, which is a library a
    /// record can neither name nor be matched against
    pub machine_id: &'a str,
    pub key: i64,
}

/// The effective order of `default` (libraries in the server's own order) under `record`: a
/// permutation, each entry an index into `default`, first shown first.
pub fn effective(default: &[Lib<'_>], record: Option<&LibraryOrder>) -> Vec<usize> {
    let mut placed: Vec<usize> = Vec::with_capacity(default.len());
    let mut is_placed = vec![false; default.len()];
    if let Some(record) = record {
        for named in &record.libs {
            if named.machine_id.is_empty() {
                continue;
            }
            let found = default.iter().position(|lib| lib.machine_id == named.machine_id && lib.key == named.key);
            if let Some(index) = found.filter(|index| !is_placed[*index]) {
                is_placed[index] = true;
                placed.push(index);
            }
        }
    }
    // In default order, so a run of unnamed libraries chains behind the nearest named one: each
    // one's predecessor (`index - 1`) is already placed when it is reached.
    for index in 0..default.len() {
        if is_placed[index] {
            continue;
        }
        let at = match index.checked_sub(1) {
            Some(before) => placed.iter().position(|placed| *placed == before).map_or(placed.len(), |at| at + 1),
            None => 0,
        };
        placed.insert(at, index);
        is_placed[index] = true;
    }
    placed
}

/// Is `order` (from [`effective`]) the default order itself?
pub fn is_server_order(order: &[usize]) -> bool {
    order.iter().enumerate().all(|(at, index)| at == *index)
}

/// Which library a `/hubs` shelf belongs to, from its `hubIdentifier`: the per-library Recently
/// Added family (`movie.recentlyadded.<id>`, `show.recentlyadded.<id>`, `tv.recentlyadded.<id>`) and
/// a promoted collection (`custom.collection.<section>.…`). `None` for every other identifier —
/// among them the whole-server `home.*` ones, which span libraries by construction.
pub fn hub_library(hub_identifier: &str) -> Option<i64> {
    const PER_LIBRARY: [&str; 3] = ["movie.recentlyadded.", "show.recentlyadded.", "tv.recentlyadded."];
    for prefix in PER_LIBRARY {
        if let Some(rest) = hub_identifier.strip_prefix(prefix) {
            return rest.parse().ok();
        }
    }
    hub_identifier.strip_prefix("custom.collection.")?.split('.').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plex::session::OrderedLib;

    fn libs() -> Vec<Lib<'static>> {
        // own server A (keys 4, 1, 5 in the server's own order), then a friend's B (keys 2, 9)
        vec![
            Lib { machine_id: "A", key: 4 }, Lib { machine_id: "A", key: 1 }, Lib { machine_id: "A", key: 5 },
            Lib { machine_id: "B", key: 2 }, Lib { machine_id: "B", key: 9 },
        ]
    }
    fn record(named: &[(&str, i64)]) -> LibraryOrder {
        LibraryOrder {
            user: String::new(),
            libs: named.iter().map(|(machine_id, key)| OrderedLib {
                machine_id: (*machine_id).into(), key: *key, extensions: Default::default() }).collect(),
            extensions: Default::default(),
        }
    }

    #[test]
    fn no_record_is_the_server_order() {
        let order = effective(&libs(), None);
        assert_eq!(order, vec![0, 1, 2, 3, 4]);
        assert!(is_server_order(&order));
        assert!(is_server_order(&effective(&libs(), Some(&record(&[])))), "an empty record says nothing");
    }

    #[test]
    fn a_recorded_order_ranks_across_servers() {
        // a friend's share sits between the owner's own libraries
        let order = effective(&libs(), Some(&record(&[("A", 1), ("B", 2), ("A", 4), ("A", 5), ("B", 9)])));
        assert_eq!(order, vec![1, 3, 0, 2, 4]);
        assert!(!is_server_order(&order));
    }

    #[test]
    fn a_library_the_record_does_not_name_falls_after_its_default_predecessor() {
        // A/5 and B/9 are new: A/5's predecessor is A/1 (which the record moved to the front), and
        // B/9's is B/2
        let order = effective(&libs(), Some(&record(&[("A", 1), ("B", 2), ("A", 4)])));
        assert_eq!(order, vec![1, 2, 3, 4, 0]);
    }

    #[test]
    fn an_unnamed_library_with_no_predecessor_leads() {
        let order = effective(&libs(), Some(&record(&[("B", 9), ("B", 2)])));
        // A/4 has no predecessor; A/1 and A/5 chain behind it; the named two keep their order
        assert_eq!(order, vec![0, 1, 2, 4, 3]);
    }

    #[test]
    fn a_recorded_library_that_is_absent_costs_nothing() {
        let present = &libs()[..3];
        let order = effective(present, Some(&record(&[("B", 2), ("A", 5), ("A", 4)])));
        assert_eq!(order, vec![2, 0, 1], "A/1 is unnamed: it follows A/4 (its predecessor)");
    }

    #[test]
    fn a_library_with_no_machine_id_is_never_matched_by_the_record() {
        let anonymous = [Lib { machine_id: "", key: 1 }, Lib { machine_id: "", key: 2 }];
        let order = effective(&anonymous, Some(&record(&[("", 2), ("", 1)])));
        assert_eq!(order, vec![0, 1], "the record cannot name them, so the default stands");
    }

    #[test]
    fn a_duplicate_entry_places_its_library_once() {
        let order = effective(&libs(), Some(&record(&[("A", 5), ("A", 5), ("A", 4)])));
        assert_eq!(order.len(), 5);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![0, 1, 2, 3, 4], "a permutation");
    }

    #[test]
    fn hub_library_names_only_what_the_identifier_names() {
        assert_eq!(hub_library("movie.recentlyadded.3"), Some(3));
        assert_eq!(hub_library("tv.recentlyadded.12"), Some(12));
        assert_eq!(hub_library("show.recentlyadded.7"), Some(7));
        assert_eq!(hub_library("custom.collection.2.420.420"), Some(2));
        assert_eq!(hub_library("custom.collection.x.420.420"), None);
        assert_eq!(hub_library("home.movies.recent"), None, "a whole-server hub spans libraries");
        assert_eq!(hub_library("home.ondeck"), None);
        assert_eq!(hub_library("movie.recentlyadded.x"), None);
    }
}
