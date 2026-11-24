use super::*;

const T: Duration = Duration::from_millis(10);

#[test]
fn full_queue_drops_oldest_and_reports_count() {
    let q = DropOldest::new(2);
    for i in 0..5 {
        q.push(i);
    }
    assert_eq!(q.pop(T), Pop::Item(3, 3));
    assert_eq!(q.pop(T), Pop::Item(4, 0));
    assert_eq!(q.total_dropped(), 3);
}

#[test]
fn kept_items_are_never_evicted() {
    let q = DropOldest::new(2);
    q.push_keep(100);
    for i in 0..5 {
        q.push(i);
    }
    assert_eq!(q.pop(T), Pop::Item(100, 4), "控制消息留在队首");
    assert_eq!(q.pop(T), Pop::Item(4, 0));
    assert_eq!(q.pop(T), Pop::Timeout);
}

#[test]
fn empty_pop_times_out() {
    let q: DropOldest<u8> = DropOldest::new(1);
    assert_eq!(q.pop(T), Pop::Timeout);
}

#[test]
fn close_drains_then_reports_closed() {
    let q = DropOldest::new(4);
    q.push(1);
    q.close();
    assert!(!q.push(2));
    assert_eq!(q.pop(T), Pop::Item(1, 0));
    assert_eq!(q.pop(T), Pop::Closed);
}

#[test]
fn pop_wakes_on_push_from_other_thread() {
    let q = std::sync::Arc::new(DropOldest::new(1));
    let q2 = q.clone();
    let h = std::thread::spawn(move || q2.pop(Duration::from_secs(5)));
    std::thread::sleep(T);
    q.push(7);
    assert_eq!(h.join().unwrap(), Pop::Item(7, 0));
}
