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

fn scope<'a>(order: &'a [(ServerId, i64)]) -> BrowseScope<'a> {
    BrowseScope { order, ..BrowseScope::standalone() }
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
/// library-less shelf ahead of every library, and a shelf with no recognised hub id following its
/// cards' one library.
#[test]
fn a_record_ranks_shelves_across_sources() {
    let srcs = sources();
    let order = [(sid(1), 7), (sid(0), 2), (sid(0), 1)];
    let (cat, hubs, _) = merge_with_scope(&srcs, &scope(&order));
    assert_eq!(ids(&hubs), ["home.playlists", "movie.recentlyadded.7", "tv.recentlyadded.2", "mock.shelf.0", "movie.recentlyadded.1"]);
    assert_eq!(cat[hubs[1].start].sid, sid(1), "the friend's row holds the friend's cards");
    assert_eq!(cat[hubs[4].start].rk, "movie.recentlyadded.1-a", "and a row still points at its own cards");
}

/// A shelf with no library of its own inherits the rank of the shelf before it in ITS source.
#[test]
fn a_library_less_shelf_follows_the_shelf_before_it_in_its_source() {
    let mut srcs = sources();
    srcs[0].last.as_mut().unwrap().shelves.swap(0, 1); // recentlyadded.1, then the playlists
    let order = [(sid(0), 2), (sid(1), 7), (sid(0), 1)];
    let (_, hubs, _) = merge_with_scope(&srcs, &scope(&order));
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
    assert_eq!(pool(&scope(&order)), pool(&BrowseScope::standalone()));
}

/// A row that gave its cards up to the ring keeps its rank: the shown order is the same before and
/// after the ring takes the cards (the library is read at projection, not from the cards it holds).
#[test]
fn a_released_row_keeps_its_place() {
    let mut srcs = sources();
    for shelf in &mut srcs[0].last.as_mut().unwrap().shelves { shelf.library = derive_library(&shelf.hub_id, &shelf.items); }
    let order = [(sid(1), 7), (sid(0), 2), (sid(0), 1)];
    let before = ids(&merge_with_scope(&srcs, &scope(&order)).1);
    let mixed = &mut srcs[0].last.as_mut().unwrap().shelves[3];
    mixed.shown = mixed.items.len();
    mixed.items = Vec::new();
    assert!(mixed.released());
    assert_eq!(before, ids(&merge_with_scope(&srcs, &scope(&order)).1));
}

/// The ring reads the same order the merge publishes, so a row's index names its own shelf.
#[test]
fn the_ring_maps_rows_in_the_shown_order() {
    let srcs = sources();
    let order = [(sid(1), 7), (sid(0), 2), (sid(0), 1)];
    let published = published_order(&srcs, &scope(&order));
    let (_, hubs, _) = merge_with_scope(&srcs, &scope(&order));
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
    let key = |order: &[(ServerId, i64)]| BrowseScope { pins: &pins, order, sections_gen: 4, ..BrowseScope::standalone() }.cache_key();
    assert_ne!(key(&[]), key(&[(sid(1), 7), (sid(0), 1)]));
    assert_ne!(key(&[(sid(1), 7), (sid(0), 1)]), key(&[(sid(0), 1), (sid(1), 7)]));
    assert_eq!(key(&[(sid(0), 1)]), key(&[(sid(0), 1)]));
}
