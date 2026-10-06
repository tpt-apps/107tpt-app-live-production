//! Real-time-safety proofs (spec 3.1, 21): the render paths must not
//! allocate while a show runs.
//!
//! `tpt-av-test-benchmark`'s tracking allocator is this binary's global
//! allocator (it installs itself on link). We assert with the **global**
//! allocation counters: the thread-local scope helper records phantom
//! counts for closures that touch per-call TLS, but the global counter is
//! ground truth. Measured regions are single-threaded loops (the test
//! harness's other threads are idle), so global deltas are exact.
//! Setup allocations are allowed; per-tick/per-block allocations are not.

use std::time::Duration;
use tpt_app_live_production_mixer::{Mixer, RenderInputs};
use tpt_app_live_production_model::cue::Transition;
use tpt_app_live_production_model::ids::{BusId, SourceId};
use tpt_app_live_production_switcher::Switcher;
use tpt_av_test_benchmark::allocation_tracker::{
    global_allocation_count, global_deallocation_count,
};

/// Runs `f` and asserts it performed no heap allocations or deallocations.
fn assert_zero_allocations<R>(context: &str, f: impl FnOnce() -> R) -> R {
    let before_alloc = global_allocation_count();
    let before_dealloc = global_deallocation_count();
    let result = f();
    let allocs = global_allocation_count() - before_alloc;
    let deallocs = global_deallocation_count() - before_dealloc;
    assert_eq!(
        allocs, 0,
        "{context}: {allocs} allocations on the render path (spec 3.1 violation)"
    );
    assert_eq!(
        deallocs, 0,
        "{context}: {deallocs} deallocations on the render path (spec 3.1 violation)"
    );
    result
}

#[test]
fn mixer_render_block_allocates_nothing() {
    let mut mixer = Mixer::new(48_000, 128, Duration::from_millis(25)).unwrap();
    mixer.add_bus(BusId::new("program")).unwrap();
    mixer.add_bus(BusId::new("aux")).unwrap();
    let mic = SourceId::new("mic1");
    mixer.add_source(mic.clone()).unwrap();
    mixer.patch(&mic, &BusId::new("program")).unwrap();

    let input = vec![0.25f32; 128 * 2];
    let mut program = vec![0.0f32; 128 * 2];
    let mut aux = vec![0.0f32; 128 * 2];

    // Host-side input assembly happens once per reconfiguration, not per
    // block.
    let mut inputs = RenderInputs::new();
    inputs.push(&mic, &input);

    for _ in 0..16 {
        let mut outs: [&mut [f32]; 2] = [&mut program, &mut aux];
        mixer.render_block(&inputs, &mut outs);
    }

    for _ in 0..100 {
        assert_zero_allocations("mixer render_block", || {
            let mut outs: [&mut [f32]; 2] = [&mut program, &mut aux];
            mixer.render_block(&inputs, &mut outs);
        });
    }
}

#[test]
fn switcher_tick_and_cut_allocate_nothing() {
    let mut switcher = Switcher::new(60, SourceId::new("cam1")).unwrap();
    switcher.register_source(SourceId::new("cam1"));
    switcher.register_source(SourceId::new("cam2"));

    // The render host owns one frame buffer and reuses it every frame.
    let mut frame = switcher.tick(0);
    for i in 1..16u64 {
        switcher.select_preview(SourceId::new("cam2")).unwrap();
        switcher.take(&Transition::Cut, i).unwrap();
        switcher.tick_into(i, &mut frame);
    }

    // select_preview takes ownership by move, so the host rotates two
    // pre-constructed handles: the outgoing preview comes back as the next
    // handle and no ids are constructed inside the measured loop.
    let mut handle = SourceId::new("cam2");
    for i in 100..200u64 {
        assert_zero_allocations("switcher cut + tick_into", || {
            let incoming = std::mem::replace(&mut handle, switcher.preview().clone());
            switcher.select_preview(incoming).unwrap();
            switcher.take(&Transition::Cut, i).unwrap();
            switcher.tick_into(i, &mut frame);
        });
    }
}

#[test]
fn switcher_fade_frames_allocate_nothing_in_steady_state() {
    let mut switcher = Switcher::new(60, SourceId::new("cam1")).unwrap();
    switcher.register_source(SourceId::new("cam1"));
    switcher.register_source(SourceId::new("cam2"));

    let mut frame = switcher.tick(0);
    // One full fade preallocates the frame buffer's transition strings.
    switcher.select_preview(SourceId::new("cam2")).unwrap();
    switcher
        .take(
            &Transition::Fade {
                duration: Duration::from_millis(160),
            },
            0,
        )
        .unwrap();
    for i in 0..12u64 {
        switcher.tick_into(i, &mut frame);
    }

    // A TAKE is one bounded operator action (its ActiveTransition
    // allocates at operator frequency, not frame frequency) - the gate
    // covers the per-frame path.
    switcher.select_preview(SourceId::new("cam1")).unwrap();
    switcher
        .take(
            &Transition::Fade {
                duration: Duration::from_millis(160),
            },
            100,
        )
        .unwrap();
    for i in 100..112u64 {
        assert_zero_allocations("switcher mid-fade tick_into", || {
            switcher.tick_into(i, &mut frame);
        });
    }
}

#[test]
fn ramped_gain_change_on_render_path_allocates_nothing() {
    let mut mixer = Mixer::new(48_000, 128, Duration::from_millis(25)).unwrap();
    mixer.add_bus(BusId::new("program")).unwrap();
    let mic = SourceId::new("mic1");
    mixer.add_source(mic.clone()).unwrap();
    mixer.patch(&mic, &BusId::new("program")).unwrap();
    let input = vec![0.25f32; 128 * 2];
    let mut program = vec![0.0f32; 128 * 2];

    let mut inputs = RenderInputs::new();
    inputs.push(&mic, &input);

    for _ in 0..16 {
        mixer
            .set_gain(&mic, -6.0, Duration::from_millis(25))
            .unwrap();
        let mut outs: [&mut [f32]; 1] = [&mut program];
        mixer.render_block(&inputs, &mut outs);
    }

    // Cue-triggered ramps + render together: still allocation-free.
    for _ in 0..50 {
        assert_zero_allocations("ramped gain + render", || {
            mixer
                .set_gain(&mic, -12.0, Duration::from_millis(25))
                .unwrap();
            mixer
                .set_gain(&mic, 0.0, Duration::from_millis(25))
                .unwrap();
            let mut outs: [&mut [f32]; 1] = [&mut program];
            mixer.render_block(&inputs, &mut outs);
        });
    }
}
