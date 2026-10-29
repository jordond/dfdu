use super::{Count, Model, Timing};
use std::time::Duration;

#[test]
fn timing_buckets_hold_their_durations() {
    let mut last = 0;
    for us in 0..100_000u64 {
        let d = Duration::from_micros(us);
        let k = Timing::bucket(d);
        assert!(k == last || k == last + 1, "{us} µs");
        last = k;
        assert!(d < Timing::upper(k), "{us} µs");
        assert!(k == 0 || d >= Timing::upper(k - 1), "{us} µs");
    }
    let mut h = [0; Timing::BUCKETS];
    h[Timing::bucket(Duration::from_micros(2047))] = 1;
    h[Timing::bucket(Duration::from_micros(2048))] = 10;
    assert_eq!(Timing::at_least(&h, Duration::from_millis(2)), 10);
    assert_eq!(
        Timing::bucket(Duration::from_secs(1 << 40)),
        Timing::BUCKETS - 1
    );
}

#[test]
fn fingerprint_changes_with_what_readers_show() {
    let mut m = Model::default();
    for _ in 0..100 {
        m.status.notice("n".into());
    }
    let mut last = m.fingerprint();
    let mut changed = |m: &Model, what: &str| {
        let now = m.fingerprint();
        assert_ne!(now, last, "{what}");
        last = now;
    };
    m.status.notice("n".into());
    changed(&m, "a notice when 100 are kept");
    m.status.fatal = Some("panic".into());
    changed(&m, "fatal");
    m.status.disconnected = Some("eof".into());
    changed(&m, "disconnected");
    m.status.unmeasured += 1;
    changed(&m, "unmeasured");
    m.status.exact_sizes = true;
    changed(&m, "exact_sizes");
    m.status.compressed_upper_bound = true;
    changed(&m, "compressed_upper_bound");
    m.status.profile = Some((dfdu_sys::Profile::Raid1, 2.0));
    changed(&m, "profile");
    m.status.anomaly("a".into());
    changed(&m, "anomalies");
    m.native_sizes.insert(dfdu_tree::RuleIx(0), Ok(1));
    changed(&m, "native_sizes");
    m.status.staged.add(10);
    changed(&m, "staged");
}

#[test]
fn count_removes_down_to_zero() {
    let mut c = Count::default();
    c.add(10);
    c.add(5);
    assert_eq!(
        c,
        Count {
            items: 2,
            bytes: 15
        }
    );
    c.remove(20);
    assert_eq!(c, Count { items: 1, bytes: 0 });
    c.remove(1);
    c.remove(1);
    assert_eq!(c, Count::default());
}
