//! The deadline for the model's first word, as the driver times it on its own clock.
//!
//! Each test keeps its own clock, so tests running at once do not move each other's time.

use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

/// Starts a session and waits until the prompt went out.
fn prompted(rig: &mut Rig) {
    rig.driver
        .start("s1", Vec::new(), BTreeMap::new(), "hello".into());
    rig.say(&json!({"type":"ready"}));
    rig.pump_until("the prompt", |_, sent| !of_type(sent, "prompt").is_empty());
}

#[test]
fn a_silent_model_is_late_at_the_deadline_and_its_first_word_clears_it() {
    static NOW: AtomicU64 = AtomicU64::new(1);
    fn now() -> u64 {
        NOW.load(Ordering::SeqCst)
    }
    let mut rig = Rig::with_clock(now);
    prompted(&mut rig);

    NOW.fetch_add(FIRST_REPLY_MS - 1, Ordering::SeqCst);
    rig.driver.pump();
    assert!(!rig.driver.first_reply_late());

    NOW.fetch_add(1, Ordering::SeqCst);
    assert!(rig.driver.pump(), "the change must redraw with no event");
    assert!(rig.driver.first_reply_late());

    rig.say(&json!({"type":"message_update",
        "assistantMessageEvent":{"type":"thinking_delta","delta":"Let me look"}}));
    rig.pump_until("the first word", |d, _| !d.first_reply_late());
}

#[test]
fn omps_retry_starts_a_new_deadline_after_its_delay() {
    static NOW: AtomicU64 = AtomicU64::new(1);
    fn now() -> u64 {
        NOW.load(Ordering::SeqCst)
    }
    let mut rig = Rig::with_clock(now);
    prompted(&mut rig);
    NOW.fetch_add(FIRST_REPLY_MS, Ordering::SeqCst);
    rig.driver.pump();
    assert!(rig.driver.first_reply_late());

    rig.say(
        &json!({"type":"auto_retry_start","attempt":2,"maxAttempts":3,"delayMs":4000,
        "errorMessage":"529 overloaded"}),
    );
    rig.pump_until("the retry", |d, _| !d.first_reply_late());

    NOW.fetch_add(FIRST_REPLY_MS + 3_999, Ordering::SeqCst);
    rig.driver.pump();
    assert!(!rig.driver.first_reply_late(), "omp's own delay counted");
    NOW.fetch_add(1, Ordering::SeqCst);
    rig.driver.pump();
    assert!(rig.driver.first_reply_late());
}

#[test]
fn a_stop_ends_the_wait() {
    static NOW: AtomicU64 = AtomicU64::new(1);
    fn now() -> u64 {
        NOW.load(Ordering::SeqCst)
    }
    let mut rig = Rig::with_clock(now);
    prompted(&mut rig);
    NOW.fetch_add(FIRST_REPLY_MS, Ordering::SeqCst);
    rig.driver.pump();
    assert!(rig.driver.first_reply_late());

    assert!(rig.driver.stop());
    rig.driver.pump();
    assert!(
        !rig.driver.first_reply_late(),
        "a stop was already asked for"
    );
    NOW.fetch_add(10 * FIRST_REPLY_MS, Ordering::SeqCst);
    rig.driver.pump();
    assert!(!rig.driver.first_reply_late());
}
