rusty_h264-decoder 0.16.0 from crates.io (BSD-2-Clause, see LICENSE),
the H.264 decoder behind the Video Player's MP4 support.

Changed for RyzikOS: `src/mb16.rs` names `alloc::vec::Vec` in one line of
a statistics printout, so the crate builds without `std` (upstream only
builds that line with `std`). Examples and tests were left out.
