//! **Library order on Home** (Settings, LO-2): with a record the merge stable-sorts every source's
//! shelves, across sources, by their library's rank; with none, Home is exactly what it was. The
//! ring (`published_order`), the hero pool and a released row all have to agree with that.

use super::*;
use super::test_support::*;

/// Own server: a library-less whole-server hub, then library 1's and library 2's Recently Added, then
/// an unrecognised hub whose cards all sit in library 2. The friend's: library 7's Recently Added.
fn sources() -> [Src; 2] {
    let recent = |slot: u16, sec: i64, id: &str| {
        let mut shelf = shelf_in(slot, sec, id, id, &[&format!("{id}-a"), &format!("{id}-b")]);
        for card in &mut shelf.items { Arc::make_mut(card).art = "/art".into(); }
        shelf
    };
    [
        src(0, "", HubState::Ready, Some(built(0, &[], vec![
            shelf(0, "Playlists", "home.playlists", &["pl"]),
            recent(0, 1, "movie.recentlyadded.1"),
            recent(0, 2, "tv.recentlyadded.2"),
            shelf_in(0, 2, "Mixed", "mock.shelf.0", &["m0", "m1"]),
        ]))),
        src(1, "friend", HubState::Ready, Some(built(1, &[], vec![recent(1, 7, "movie.recentlyadded.7")]))),
    ]
}

fn ids(hubs: &[HubRow]) -> Vec<String> { hubs.iter().map(|hub| hub.hub_id.clone()).collect() }

const SERVER_ORDER: [&str; 5] = ["home.playlists", "movie.recentlyadded.1", "tv.recentlyadded.2", "mock.shelf.0", "movie.recentlyadded.7"];

/// The server's own order of `effective`'s libraries in these fixtures: by server, then library key.
fn defaults(effective: &[(ServerId, i64)]) -> Vec<(ServerId, i64)> {
    let mut default = effective.to_vec();
    default.sort_by_key(|(sid, key)| (sid.raw(), *key));
    default
}

fn scope<'a>(effective: &'a [(ServerId, i64)], default: &'a [(ServerId, i64)]) -> BrowseScope<'a> {
    BrowseScope { order: HomeOrder::Moved { effective, default }, ..BrowseScope::standalone() }
}

/// No record: the merge is the server order, source by source, exactly what it was.
#[test]
fn without_a_record_home_is_the_server_order() {
    let srcs = sources();
    let (_, hubs, _) = merge_with_scope(&srcs, &BrowseScope::standalone());
    assert_eq!(ids(&hubs), SERVER_ORDER);
    assert_eq!(published_order(&srcs, &BrowseScope::standalone()), natural_order(&srcs, &[]));
}

/// A record ranks shelves across sources: the friend's library between the owner's own, a
/// library-less shelf staying with the first shelf of ITS source, and a shelf with no recognised hub
/// id following its cards' one library (decided at projection).
#[test]
fn a_record_ranks_shelves_across_sources() {
    let srcs = sources();
    let order = [(sid(1), 7), (sid(0), 2), (sid(0), 1)];
    let (cat, hubs, _) = merge_with_scope(&srcs, &scope(&order, &defaults(&order)));
    assert_eq!(ids(&hubs), ["movie.recentlyadded.7", "home.playlists", "tv.recentlyadded.2", "mock.shelf.0", "movie.recentlyadded.1"],
        "the owner's Playlists lead the owner's own shelves, not the page");
    assert_eq!(cat[hubs[0].start].sid, sid(1), "the friend's row holds the friend's cards");
    assert_eq!(cat[hubs[4].start].rk, "movie.recentlyadded.1-a", "and a row still points at its own cards");
}

/// A shelf with no library of its own inherits the rank of the shelf before it in ITS source.
#[test]
fn a_library_less_shelf_follows_the_shelf_before_it_in_its_source() {
    let mut srcs = sources();
    srcs[0].last.as_mut().unwrap().shelves.swap(0, 1); // recentlyadded.1, then the playlists
    let order = [(sid(0), 2), (sid(1), 7), (sid(0), 1)];
    let (_, hubs, _) = merge_with_scope(&srcs, &scope(&order, &defaults(&order)));
    assert_eq!(ids(&hubs), ["tv.recentlyadded.2", "mock.shelf.0", "movie.recentlyadded.7", "movie.recentlyadded.1", "home.playlists"],
        "Playlists rides behind library 1, the shelf before it");
}

/// The hero pool is built in the server's order whatever order Home shows.
#[test]
fn the_hero_pool_does_not_move_with_the_order() {
    let srcs = sources();
    let pool = |scope: &BrowseScope| {
        let (cat, _, pool) = merge_with_scope(&srcs, scope);
        pool.iter().map(|slot| cat[slot.idx].rk.clone()).collect::<Vec<_>>()
    };
    let order = [(sid(1), 7), (sid(0), 2), (sid(0), 1)];
    assert!(!pool(&BrowseScope::standalone()).is_empty());
    assert_eq!(pool(&scope(&order, &defaults(&order))), pool(&BrowseScope::standalone()));
}

/// A row that gave its cards up to the ring keeps its rank: the shown order is the same before and
/// after the ring takes the cards (the library is read at projection, not from the cards it holds).
#[test]
fn a_released_row_keeps_its_place() {
    let mut srcs = sources();
    for shelf in &mut srcs[0].last.as_mut().unwrap().shelves { shelf.library = ShelfLibrary::projected(&shelf.hub_id, &shelf.items); }
    let order = [(sid(1), 7), (sid(0), 2), (sid(0), 1)];
    let before = ids(&merge_with_scope(&srcs, &scope(&order, &defaults(&order))).1);
    let mixed = &mut srcs[0].last.as_mut().unwrap().shelves[3];
    mixed.shown = mixed.items.len();
    mixed.items = Vec::new();
    assert!(mixed.released());
    assert_eq!(before, ids(&merge_with_scope(&srcs, &scope(&order, &defaults(&order))).1));
}

/// The ring reads the same order the merge publishes, so a row's index names its own shelf.
#[test]
fn the_ring_maps_rows_in_the_shown_order() {
    let srcs = sources();
    let order = [(sid(1), 7), (sid(0), 2), (sid(0), 1)];
    let published = published_order(&srcs, &scope(&order, &defaults(&order)));
    let (_, hubs, _) = merge_with_scope(&srcs, &scope(&order, &defaults(&order)));
    let deck = hubs.len() - published.len();
    for (at, (i, j)) in published.iter().enumerate() {
        assert_eq!(hubs[deck + at].hub_id, srcs[*i].last.as_ref().unwrap().shelves[*j].hub_id);
    }
}

/// A moved Library order is a moved Home even when no pin and no generation moved with it, so the
/// published Home is rebuilt from the sources already held, with no refetch.
#[test]
fn a_moved_order_alone_moves_the_scope() {
    let pins = [(sid(0), 1, true), (sid(1), 7, true)];
    let (moved, server) = ([(sid(1), 7), (sid(0), 1)], [(sid(0), 1), (sid(1), 7)]);
    let key = |order: HomeOrder<&[(ServerId, i64)]>| BrowseScope { pins: &pins, order, sections_gen: 4, ..BrowseScope::standalone() }.cache_key();
    let moved_order = HomeOrder::Moved { effective: &moved[..], default: &server[..] };
    assert_ne!(key(HomeOrder::Natural), key(moved_order));
    assert_ne!(key(moved_order), key(HomeOrder::Moved { effective: &server[..], default: &server[..] }));
    assert_eq!(key(moved_order), key(HomeOrder::Moved { effective: &moved[..], default: &server[..] }));
}

// ---- review findings: one definition of "natural", unranked and released rows ---------------------

/// Own server `home`: two Recently Added shelves in /hubs order `tv.2, movie.1` (the directory lists
/// library 1 first). A friend `anna`: On Deck first, then `movie.7`, `movie.8`.
fn two_sources() -> [Src; 2] {
    let recent = |slot: u16, sec: i64, id: &str| {
        let mut shelf = shelf_in(slot, sec, id, id, &[&format!("{id}-a")]);
        for card in &mut shelf.items { Arc::make_mut(card).art = "/art".into(); }
        shelf
    };
    [
        src(0, "", HubState::Ready, Some(built(0, &[], vec![recent(0, 2, "tv.recentlyadded.2"), recent(0, 1, "movie.recentlyadded.1")]))),
        src(1, "anna", HubState::Ready, Some(built(1, &[], vec![
            shelf(1, "On Deck", "home.ondeck", &["od"]), recent(1, 7, "movie.recentlyadded.7"), recent(1, 8, "movie.recentlyadded.8")]))),
    ]
}

/// F2: a reorder on the friend's libraries alone leaves the owner's shelves exactly as /hubs had
/// them, and the friend's leading On Deck stays with the friend.
#[test]
fn a_reorder_on_one_source_leaves_the_other_sources_shelves_natural() {
    let srcs = two_sources();
    // the user swapped the friend's two libraries; everything else is the server order
    let order = [(sid(0), 1), (sid(0), 2), (sid(1), 8), (sid(1), 7)];
    let (_, hubs, _) = merge_with_scope(&srcs, &scope(&order, &defaults(&order)));
    assert_eq!(ids(&hubs), ["tv.recentlyadded.2", "movie.recentlyadded.1", "home.ondeck", "movie.recentlyadded.8", "movie.recentlyadded.7"],
        "the owner's /hubs order is untouched and On Deck leads its own source, not the whole page");
}

/// F2: a reorder inside one source moves that source's libraries; a library-less shelf stays attached
/// to the shelf before it in ITS source whatever the other sources do.
#[test]
fn a_source_whose_library_order_is_unchanged_keeps_its_natural_shelves_when_another_moves_between_them() {
    let srcs = two_sources();
    // the friend's library 7 now sits between the owner's two: the owner's own order is unchanged
    let order = [(sid(0), 1), (sid(1), 7), (sid(0), 2), (sid(1), 8)];
    let (_, hubs, _) = merge_with_scope(&srcs, &scope(&order, &defaults(&order)));
    let shown = ids(&hubs);
    let own: Vec<_> = shown.iter().filter(|id| id.starts_with("tv.") || id.starts_with("movie.recentlyadded.1")).cloned().collect();
    assert_eq!(own, ["tv.recentlyadded.2", "movie.recentlyadded.1"], "the owner's own shelves keep /hubs order");
    let at = |id: &str| shown.iter().position(|s| s == id).unwrap();
    assert!(at("home.ondeck") < at("movie.recentlyadded.7"), "On Deck rides with its own source's first shelf");
}

/// F3: a share whose /hubs landed before its directory has no rank yet. It sits where it naturally
/// does (behind the owner's shelves) instead of jumping to the top and back when the directory lands.
#[test]
fn a_source_with_no_ranked_library_holds_its_natural_position() {
    let srcs = two_sources();
    let order = [(sid(0), 1), (sid(0), 2)]; // anna's libraries have not landed
    let (_, hubs, _) = merge_with_scope(&srcs, &scope(&order, &defaults(&order)));
    assert_eq!(ids(&hubs)[2..], ["home.ondeck", "movie.recentlyadded.7", "movie.recentlyadded.8"]);
}

/// F7: a refresh re-projects every row, and a released row's library is the one it was given when
/// it was first projected, not whatever the fresh preview's sample says.
#[test]
fn a_refresh_keeps_the_library_a_released_row_was_projected_with() {
    let mut was = shelf_in(0, 2, "Mixed", "mock.shelf.0", &["m0", "m1"]);
    was.library = ShelfLibrary::One(2);
    was.shown = was.items.len();
    was.items = Vec::new();
    let old = built(0, &[], vec![was]);
    let mut fresh = shelf_in(0, 5, "Mixed", "mock.shelf.0", &["m2", "m3"]);
    fresh.library = ShelfLibrary::projected(&fresh.hub_id, &fresh.items);
    let mut new = built(0, &[], vec![fresh]);
    carry_descriptors(&old, &mut new);
    assert_eq!(new.shelves[0].library, ShelfLibrary::One(2));
    assert!(new.shelves[0].released());
}

/// F7: a window read builds the replacement with no library of its own: the row keeps the projection's.
#[test]
fn a_window_read_keeps_the_library_of_the_row_it_replaces() {
    let mut shelf = shelf_in(0, 2, "Mixed", "mock.shelf.0", &["m0"]);
    shelf.library = ShelfLibrary::One(2);
    let old = shelf.clone();
    let mut refreshed = shelf_in(0, 5, "", "mock.shelf.0", &["m1", "m2"]);
    refreshed.library = ShelfLibrary::Unknown; // what `fetch_window` builds: it decided nothing
    adopt_window(&mut shelf, refreshed, &old);
    assert_eq!(shelf.library, ShelfLibrary::One(2));
    assert_eq!(shelf.title, "Mixed");
}

/// F7, the replay case: a row decoded from a recording has decided nothing, so it ranks by its hub id
/// alone, and gives the same place before and after it gives its cards up.
#[test]
fn a_replayed_row_ranks_the_same_before_and_after_it_gives_its_cards_up() {
    let mut srcs = two_sources();
    let replay = |shelf: &Shelf| -> Shelf { serde_json::from_str(&serde_json::to_string(shelf).unwrap()).unwrap() };
    for source in &mut srcs {
        for shelf in &mut source.last.as_mut().unwrap().shelves { *shelf = replay(shelf); }
    }
    let order = [(sid(0), 2), (sid(0), 1), (sid(1), 8), (sid(1), 7)];
    let before = ids(&merge_with_scope(&srcs, &scope(&order, &defaults(&order))).1);
    for source in &mut srcs {
        for shelf in &mut source.last.as_mut().unwrap().shelves {
            shelf.shown = shelf.items.len();
            shelf.items = Vec::new();
        }
    }
    assert_eq!(before, ids(&merge_with_scope(&srcs, &scope(&order, &defaults(&order))).1));
}
