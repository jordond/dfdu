use super::*;

fn quiescent(n: u64) -> Vec<ScanMsg> {
    (0..n)
        .map(|submitted_seq| ScanMsg::Quiescent { submitted_seq })
        .collect()
}

/// Data takes budget, clamped to the capacity, and is refused by `try_send` when the budget
/// is spent; control messages take none. Every message is counted as sent before it is
/// received, and arrives in the order it was sent.
#[test]
fn data_takes_the_budget_control_does_not_and_every_message_is_counted() {
    let (inbox, rx, budget, sent) = Inbox::new(100);
    let data = Batch {
        msgs: quiescent(2),
        bytes: 60,
    };
    assert!(inbox.try_send(data).is_ok());
    assert_eq!((budget.used(), sent.load(Relaxed)), (60, 2));
    let over = Batch {
        msgs: quiescent(1),
        bytes: 50,
    };
    let refused = inbox.try_send(over).expect_err("over the budget");
    assert_eq!((refused.bytes, refused.msgs.len()), (50, 1));
    assert_eq!((budget.used(), sent.load(Relaxed)), (60, 2));
    inbox.send_control(ScanMsg::Quiescent { submitted_seq: 9 });
    assert_eq!((budget.used(), sent.load(Relaxed)), (60, 3));

    let first = rx.try_recv().expect("data");
    assert_eq!((first.msgs.len(), first.bytes), (2, 60));
    budget.release(first.bytes);
    // A batch larger than the whole budget is clamped to it, once the budget is empty.
    inbox.send(Batch {
        msgs: quiescent(1),
        bytes: 500,
    });
    assert_eq!((budget.used(), sent.load(Relaxed)), (100, 4));
    let control = rx.try_recv().expect("control");
    assert_eq!(control.bytes, 0);
    assert!(matches!(
        control.msgs[..],
        [ScanMsg::Quiescent { submitted_seq: 9 }]
    ));
    assert_eq!(rx.try_recv().expect("clamped").bytes, 100);
    assert!(rx.try_recv().is_err());
}
