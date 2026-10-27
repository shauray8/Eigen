# Attribution

## FilmCraft

`crates/app/src/timeline.rs` ports the look and mechanics of FilmCraft's timeline
(https://github.com/storytold/filmcraft, licensed MIT OR Apache-2.0), from
`crates/ui-egui/src/panels/`:

- `timeline.rs`: layout, ruler steps, timecode ruler, playhead, track headers, clip drawing,
  waveform mesh, snapping, wheel input, eased zoom, zoom scroll bar
- `timeline_hit.rs`: hit testing, edge zones, `Reach::beats`
- `timeline_volume.rs`: volume rubber band
