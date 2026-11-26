# Owned-input V13 replay anchor

`tests/fixtures/replay/v13-owned-input-content` is a new synthetic recording, not a rewrite of an older golden. The actual simulator's `--write-synthetic-initial` producer and product recorder captured Filmography → Detail → Back → Back. The official `tools/plxnative-rec check` and `import` accepted the unchanged capture. The imported manifest adds only `anchor: true`; its recorded initial state, hash, fingerprint and segment bytes remain the producer's.

Targets and Resolve each replay all 975 recorded frames with raw exit 0, `verdict=SAME`, and all eight difference counters zero. Current V13 recordings must continue to replay; an unsupported current or unknown profile is never an expected refusal.

The four pre-V13 recordings remain byte-for-byte unchanged, including the historical fixture whose directory ends in `-current`. `tests/replay-historical-profiles.json` binds the reviewed schema-3 fingerprints 4239819007634745513 and 13512213413816145485 to their exact manifest and segment SHA-256 values. Only these fully parsed immutable artifacts receive the typed early refusal `incompatible owner-input/presentation state; requires V13 capture`. The diagnostic grants no old-profile replay authority. Unknown, modified, malformed, truncated and extra-field artifacts cannot earn an expected refusal.

The shared CI replay driver executes all six recordings in both modes: four current SAME outcomes (this anchor and `13-by-id-filmography`, two modes each) and eight separately counted historical expected refusals. Generic errors, crashes, late bootstrap output, incomplete grades and nonzero difference counters remain failures.

**Re-recorded 2026-11-17** (route pages share one anatomy: every route title, Filmography's
included, is the 48 Settings title instead of the 72 hero). The recorded behaviour changed without
moving the shape (`state_fp` 16894190109976774341): the committed anchor was observed being refused
in both modes (`replay: REFUSED — replay measurement table miss`, the title measured at a rung the
recording never measured). The flow's keys are unchanged. The anchor was recorded afresh
(`tests/focusfp.sh --rec --only 12`, the old fixture removed, `plxnative-rec import`, `anchor: true`
restored; the manifest differs only in the initial-state hash and the mock's port), and the CI driver
(`tests/replay_fixtures.py`, all five recordings) replayed it SAME in Targets and Resolve on three
consecutive runs, 1,172 frames graded, every difference counter zero. The pinned digests in
`tests/test_harness.py` follow the new bytes.

**Re-recorded again 2026-11-17, with the by-id family** (the runtime half of the accessibility
identifiers, #1432). `InputKind::ById` and its `Id` resolution row moved the recorded shape
(`state_fp` 16894190109976774341 `0xea74_3865_5115_46c5` -> 9159521251572352515
`0x7f1d_280c_909c_8603`): the committed anchor was observed being refused (`replay: REFUSED --
invalid or incompatible recording`), so it was recorded afresh the same way (`tests/focusfp.sh
--rec --only 12`, `plxnative-rec import`, `anchor: true` restored; the manifest differs only in the
fingerprint, the initial-state hash and the mock's port), and the CI driver replayed it SAME in
Targets and Resolve, 922 frames graded, every difference counter zero. The outbound guard's and the
tool's `CURRENT_FP` moved with it, and the pinned digests in `tests/test_harness.py` follow the new
bytes. No frame hash was edited by hand.

`tests/fixtures/replay/13-by-id-filmography` is the one fixture that drives the by-id tokens
(`tests/focusfp.sh` flow 13: Flow 12's Filmography page, then `focus:nav.watchlist`,
`focus:filmography.row.3` and `press:filmography.row.0`, no `@subject`, four BACK keys down to Home,
and `focus:nav.watchlist` again). It is an anchor and replays SAME in both modes, 1,209 frames, on
three consecutive runs. The first `focus:nav.watchlist` is REFUSED on purpose: the Filmography page
owns input and has no tab strip, so `a11y: refused focus nav.watchlist unknown owner=filmography` is
recorded, and it is the one by-id refusal `ALPHABET.json` lists, literally (`refusal` 1 on its `Id`
row). `focus:filmography.row.3` and the press resolve (`refusal` 0; the press is the ordinary OK
Down/Up on the first credit row and opens Detail exactly as Flow 12's `down ok` does), and the last
`focus:nav.watchlist` resolves on Home to the strip's Watchlist tab. The unwinding to Home records the
Person store's `close`, which `ALPHABET.json` lists as the `PersonCmd::Close` encoding.

Pressing a Home element (`press:nav.profile`, `press:nav.movies`, `press:home.hero.info`) aborts the
controlled recording with `replay: REFUSED -- unsupported Home application effect`, so no press is
recorded on Home. The recording is also kept short, one second between the BACKs: the loader's
decoded-recording budget (`rec/strict_json.rs`, `Budget`) refused a 1.5 MB take at load
(`record JSON/budget`, line 13918 of 15444 -- the replay's generic `invalid or incompatible
recording`), because Home frames cost about 100 KB per 100 frames.

**Re-recorded for Filmography Watchlist (#1418).** The item-menu argument adds a catalog ID,
moving the V13 census to `16067387704449319837`. Both current anchors were recorded with the
synthetic simulator using `tests/focusfp.sh --rec --only 12,13` and installed with
`plxnative-rec rerecord`. Targets and Resolve each grade 614 frames for
`v13-owned-input-content` and 792 for `13-by-id-filmography`, with all difference counters at zero.
The four historical fixtures still receive their expected refusal in both modes.

**Re-recorded for Live TV (#993) with Filmography Watchlist.** The current V13 producer fingerprint is `8363262880582770195`. The synthetic flows from `tests/focusfp.sh --rec --only 12,13` have 609 and 785 frames, respectively. The historical fixture bytes and their refusal allowlist are preserved.

**Re-recorded for the shelf's recorded library and the Home roster's household order.** Two things moved Home's recorded behaviour, and only the first moved the shape. `record::SHAPE` (the Home result's wire shape) gained `library:Option<Mixed|{One:i64}>`: a recorded shelf carries the library its row was decided for. That moved the current V13 producer fingerprint from `8363262880582770195` to `1163829971889928819` (the APP half from `0x19b7_5414_e5d4_c071` to `0x4e55_da7e_c00a_a7d5`; the frozen session-codec census keeps the record shape it was graded under, `PRE_LIBRARY_RECORD_SHAPE`). Separately, with the shape unmoved, the committed anchors already replayed DIVERGED on every frame from the first Home landing (frame 4) with every difference counter at zero: `BrowseScope::cache_key` (the Home roster's key) now folds Browse's household verdicts even before a pin table exists, so the verdict landing re-merges and republishes Home's catalog once more than before. That is a behaviour change with no shape change, which `rebaseline` would refuse for an anchor; the shape bump is what makes `plxnative-rec rerecord` the right verb. Both anchors were recorded with `MOCK_PORT=32685 tests/focusfp.sh --rec --only 12,13` and installed with `plxnative-rec rerecord`. Targets and Resolve each grade 998 frames for `v13-owned-input-content` and 1343 for `13-by-id-filmography` (the counts follow the recording host's tick pace, not the flow), all difference counters zero. The three pins (`tools/plxnative-rec`, `.claude/hooks/outbound-guard.py`, `tests/test_harness.py`) moved with it, and `ALPHABET.json` admits the one new string, the enum variant `Mixed`.

**Re-recorded for the Library order Settings page (#1554).** The page is a new `SettingsPage::LibraryOrder` (`ScreenId` 122), which adds a variant to the Settings family's recorded argument shape and nothing else: no wire shape and no recorded behaviour moved. That moved the current V13 producer fingerprint from `1163829971889928819` to `2722707214457283267` (the screen half moved, the APP half did not; `the_init_probe_is_synthetic_and_the_shape_is_pinned` pins the fingerprint, now `0x25c8_ffc9_391e_a2c3`). Both anchors were recorded afresh with `MOCK_PORT=32685 tests/focusfp.sh --rec --only 12,13` and installed with `plxnative-rec rerecord`, anchors kept. `ALPHABET.json`'s owner-word pattern gained `library-order` (the page's owner word, `registry::LIBRARY_ORDER`); no other string was new. Targets and Resolve each grade 975 frames for `v13-owned-input-content` and 1291 for `13-by-id-filmography` (the counts follow the recording host's tick pace, not the flow), all difference counters zero. The three pins (`tools/plxnative-rec`, `.claude/hooks/outbound-guard.py`, `tests/test_harness.py`) moved with it.
