# FlashRaw camera database

`data/cameras.json` is the single camera database for the whole library. It is compiled
into static Rust tables by `crates/flashraw-camera-db/build.rs`; never edit generated
Rust. Edit the JSON (or normalise it with `flashraw camera-db generate`). See
[docs/camera-db.md](../../docs/camera-db.md) for the format and tooling.

## Verification

`tools/camera_db/rederive.py` checks the entries against FlashRaw's decodes of the sample files
(the CC0 raw.pixls.us corpus and real-world files) and regenerates the fields the files state or
show: names, CFA, active area, masked areas, black and white levels, image areas, and colour
matrices from camera-written DNG files. Values the files cannot supply (colour matrices of
bodies that write no DNG, decoder parameters, entries without sample files) are kept as stored.
`flashraw camera-db coverage` prints how many entries per make carry each kind of data, and
[docs/camera-db-audit.md](../../docs/camera-db-audit.md) lists, per field group and make, what the
files reproduce, contradict or cannot derive.

Each entry's `verification` holds the number of sample files it was confirmed against, the largest
sample value observed in them, and notes.
