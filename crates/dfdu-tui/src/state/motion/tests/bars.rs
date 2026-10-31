use dfdu_tree::HostPath;

use super::*;

#[test]
fn easing_converges_and_stops() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    let mut es = vec![test_entry(0, "a", 1000), test_entry(1, "b", 200)];
    app.refresh(snap_with_alloc(es.clone(), 1), t0);
    render_buffer(&mut app, t0, W, H);
    assert_eq!(shown(&app, "b"), 0.2);
    assert_eq!(app.motion.next_frame_at(), None);

    es[1].size = 800;
    let t1 = t0 + ms(100);
    app.refresh(snap_with_alloc(es, 2), t1);
    render_buffer(&mut app, t1, W, H);
    let mut last = shown(&app, "b");
    assert!(last > 0.2 && last < 0.8, "{last}");
    let mut now = t1;
    while let Some(t) = app.motion.next_frame_at() {
        assert_eq!(t, now + FRAME);
        assert!(t < t1 + Duration::from_secs(1), "still easing after 1 s");
        now = t;
        render_buffer(&mut app, now, W, H);
        let s = shown(&app, "b");
        assert!(s >= last && s <= 0.8, "{last} then {s}");
        last = s;
    }
    assert_eq!(last, 0.8);
    assert_eq!(shown(&app, "a"), 1.0);
}

#[test]
fn entering_a_directory_does_not_animate() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    app.refresh(
        snap_with_alloc(vec![test_entry(0, "a", 1000), test_entry(1, "b", 200)], 1),
        t0,
    );
    render_buffer(&mut app, t0, W, H);
    // The same nodes in another directory, with other sizes: no bar starts from the old length.
    let mut sub = snap_with_alloc(vec![test_entry(0, "a", 100), test_entry(1, "b", 900)], 2);
    sub.dir_path = HostPath::from("/r/sub");
    sub.dir_node = Some(test_entry(98, "/r/sub", 0).node);
    sub.at_root = false;
    let t1 = t0 + ms(50);
    app.refresh(sub, t1);
    render_buffer(&mut app, t1, W, H);
    assert_eq!(shown(&app, "b"), 1.0);
    assert_eq!(shown(&app, "a"), 100.0 / 900.0);
    assert_eq!(app.motion.next_frame_at(), None);
}

#[test]
fn size_mode_change_and_resize_do_not_animate() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    let mut es = vec![test_entry(0, "a", 1000), test_entry(1, "b", 200)];
    app.refresh(snap_with_alloc(es.clone(), 1), t0);
    render_buffer(&mut app, t0, W, H);

    es[1].size = 500;
    app.size_mode = SizeMode::Apparent;
    let t1 = t0 + ms(50);
    app.refresh(snap_with_alloc(es.clone(), 2), t1);
    render_buffer(&mut app, t1, W, H);
    assert_eq!(shown(&app, "b"), 0.5);
    assert_eq!(app.motion.next_frame_at(), None);

    es[1].size = 900;
    let t2 = t0 + ms(100);
    app.refresh(snap_with_alloc(es, 3), t2);
    render_buffer(&mut app, t2, W + 20, H);
    assert_eq!(shown(&app, "b"), 0.9);
    assert_eq!(app.motion.next_frame_at(), None);
}

#[test]
fn easing_sixty_rows_costs_under_two_ms_a_frame() {
    let t0 = Instant::now();
    let mut app = animated(dark(ColorLevel::TrueColor), t0);
    let mut es: Vec<EntryData> = (0..100)
        .map(|i| test_entry(i, &format!("entry{i:03}"), 1000 + u64::from(i) * 10))
        .collect();
    app.refresh(snap_with_alloc(es.clone(), 1), t0);
    render_buffer(&mut app, t0, 120, 65);
    assert_eq!(app.list_height, 60);
    // Reversed sizes: every row moves and flashes, and every bar eases.
    for (i, e) in es.iter_mut().enumerate() {
        e.size = 5000 - i as u64 * 10;
    }
    let t1 = t0 + Duration::from_secs(3);
    app.refresh(snap_with_alloc(es, 2), t1);
    let mut buf = render_buffer(&mut app, t1, 120, 65);
    assert_eq!(app.motion.markers.len(), 60);
    let visible: Vec<usize> = app.list.rows[app.list.scroll..app.list.scroll + 60].to_vec();
    let mut costs = Vec::new();
    for n in 1..=12 {
        let now = t1 + FRAME * n;
        let t = Instant::now();
        frame(&mut app, now, &visible);
        post_process(&mut app, &mut buf, now);
        costs.push(t.elapsed());
        assert!(app.motion.easing);
        assert!(app.motion.flash_at.is_some());
    }
    costs.sort();
    let median = costs[costs.len() / 2];
    println!("motion frame with 60 rows easing and flashing: {median:?} (median of 12)");
    let limit = if cfg!(debug_assertions) {
        ms(20)
    } else {
        ms(2)
    };
    assert!(median < limit, "{costs:?}");
}
