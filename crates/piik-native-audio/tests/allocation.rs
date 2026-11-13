//! Steady-state allocation bound, measured with a per-thread counting allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use piik_native_audio::{DEFAULT_BITRATE, Encoder, FRAME_BYTES};

struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: forwards every call to the system allocator unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.with(|count| count.set(count.get() + 1));
        // SAFETY: the caller upholds GlobalAlloc's contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the caller upholds GlobalAlloc's contract.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.with(|count| count.set(count.get() + 1));
        // SAFETY: the caller upholds GlobalAlloc's contract.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn encode_steady_state_allocation_is_bounded() {
    let mut encoder = Encoder::new(DEFAULT_BITRATE).expect("encoder");
    let pcm = vec![0; FRAME_BYTES];
    encoder.encode(&pcm).expect("warm-up encode");
    let mut per_window = [0usize; 2];
    for window in &mut per_window {
        let before = ALLOCATIONS.with(Cell::get);
        for _ in 0..20 {
            encoder.encode(&pcm).expect("encode");
        }
        *window = ALLOCATIONS.with(Cell::get) - before;
    }
    // opus-pure makes a fixed 6 internal scratch allocations per frame. The wrapper itself adds none, and usage must not grow.
    let allocations = per_window[1] as f64 / 20.0;
    assert!(
        allocations <= 8.0 && per_window[0] == per_window[1],
        "steady-state Opus encoding allocated {per_window:?} objects per 20 frames"
    );
}
