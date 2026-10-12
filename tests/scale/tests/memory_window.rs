//! Spec 17.5: memory growth must track the active chunk window, not the total
//! decoded PCM.
//!
//! This is a property over audio length: *for any* length between a few
//! minutes and two hours, decoding through a bounded window must not grow this
//! process's working set beyond the budget in `thresholds.toml`. The property
//! is checked against the operating system's own resident-size accounting.
//!
//! Everything lives in one `#[test]` on purpose. Cargo runs the tests in a
//! binary concurrently, and two threads allocating hundreds of megabytes while
//! both sample the *process* working set would measure each other.

use plaudon_scale_tests::memory;
use plaudon_scale_tests::pcm::{self, ChunkWindow};
use plaudon_scale_tests::rss;
use plaudon_scale_tests::thresholds::Thresholds;
use plaudon_scale_tests::SAMPLE_RATE_HZ;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};

#[test]
fn peak_working_set_tracks_the_chunk_window_not_the_total_decoded_pcm() {
    let budget = Thresholds::load().unwrap().memory().unwrap();
    let window = ChunkWindow::DEFAULT;
    let allowed = memory::allowed_growth_bytes(budget, window);

    if rss::sample().is_none() {
        // An honest skip: no number is published and nothing is asserted.
        println!(
            "SKIPPED (no resident-set-size source): {}",
            rss::unsupported_reason()
        );
        assert_eq!(rss::source(), rss::Source::Unsupported);
        return;
    }
    println!(
        "memory: source {}, window {} bytes, budget {allowed} bytes",
        rss::source().as_str(),
        window.window_bytes()
    );

    // Property: any audio length, same bound.
    let mut runner = TestRunner::new(Config {
        cases: 12,
        failure_persistence: None,
        ..Config::default()
    });
    runner
        .run(&(2u64..=120u64), |minutes| {
            let observation = memory::observe_windowed(minutes, window)
                .expect("a host that reported RSS once must keep reporting it");
            prop_assert_eq!(observation.audio_minutes, minutes);
            prop_assert_eq!(
                observation.total_pcm_bytes,
                window.total_pcm_bytes(pcm::frames_for_minutes(minutes, SAMPLE_RATE_HZ))
            );
            if let Some(reason) = memory::breach(&observation, allowed, window) {
                return Err(TestCaseError::fail(reason));
            }
            Ok(())
        })
        .expect("bounded decode must stay inside its memory budget for any audio length");

    // The same bound must hold for the longest case the spec cares about: a
    // one-hour recording, where a non-windowed decoder would hold 220 MiB.
    let hour = memory::observe_windowed(60, window).expect("hourly observation");
    assert!(
        memory::breach(&hour, allowed, window).is_none(),
        "{}",
        memory::breach(&hour, allowed, window).unwrap_or_default()
    );
    assert!(
        hour.peak_growth_bytes * 4 < hour.total_pcm_bytes,
        "peak growth {} is within a factor of four of the {} bytes of decoded PCM, which is not \
         a bounded window",
        hour.peak_growth_bytes,
        hour.total_pcm_bytes
    );

    // Growth must not scale with length: the longest run may not cost
    // materially more working set than the shortest one.
    let short = memory::observe_windowed(2, window).expect("short observation");
    assert!(
        hour.peak_growth_bytes <= short.peak_growth_bytes + allowed,
        "growth scaled with audio length: {} bytes for 2 min, {} bytes for 60 min",
        short.peak_growth_bytes,
        hour.peak_growth_bytes
    );

    // The control proves the assertion above is detectable rather than
    // vacuous: retaining every decoded frame must show up as growth.
    let control = memory::observe_unbounded(15, window).expect("control observation");
    #[allow(clippy::cast_precision_loss)]
    let detected = control.peak_growth_bytes as f64 / control.total_pcm_bytes as f64;
    assert!(
        detected >= budget.control_detection_ratio,
        "the harness only saw {:.1}% of the {} bytes the unbounded control retained; it is not \
         sensitive enough to prove the windowed bound",
        detected * 100.0,
        control.total_pcm_bytes
    );
    assert!(
        memory::breach(&control, allowed, window).is_some(),
        "the unbounded control passed the budget, so the budget proves nothing"
    );
    println!(
        "memory: control retained {} bytes of PCM and grew the working set by {} bytes",
        control.total_pcm_bytes, control.peak_growth_bytes
    );
}

#[test]
fn the_memory_budget_is_strict_enough_to_fail_an_unbounded_decoder() {
    // A budget check that does not need the host: with the committed budget, a
    // one-hour unbounded decode could never pass, whatever the machine.
    let budget = Thresholds::load().unwrap().memory().unwrap();
    let window = ChunkWindow::DEFAULT;
    let allowed = memory::allowed_growth_bytes(budget, window);
    let hour = window.total_pcm_bytes(pcm::frames_for_minutes(60, SAMPLE_RATE_HZ));
    assert!(
        allowed * 4 < hour,
        "budget {allowed} is not meaningfully below the {hour} bytes an unbounded decode holds"
    );
    assert!(
        allowed > window.window_bytes(),
        "budget must leave room for at least one full window"
    );
}
