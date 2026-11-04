//! The live text mode prints decoded lines as the drain runs.

use rtlog_decode::stdout::TextTransport;

#[test]
fn text_transport_prints_records_spans_and_names_live() {
    let contract = rtlog_decode::stdout::own_contract();
    std::thread::Builder::new()
        .name("motor".into())
        .spawn(|| {
            let _s = rtlog::span!("pid.update");
            rtlog::info!("motor speed={} rpm", 1200u16);
            rtlog::warn!("temp={} limit={}", 81u8, 85u8);
        })
        .unwrap()
        .join()
        .unwrap();
    let mut d = rtlog::Drain::new(TextTransport::new(contract, Vec::new(), false));
    d.poll().unwrap();
    let text = String::from_utf8(d.transport().get_ref().clone()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "{text}");
    assert!(lines[0].ends_with("[motor] SPAN  ▶ pid.update"), "{text}");
    assert!(
        lines[1].ends_with("[motor] INFO    motor speed=1200 rpm"),
        "{text}"
    );
    assert!(
        lines[2].ends_with("[motor] WARN    temp=81 limit=85"),
        "{text}"
    );
    assert!(lines[3].contains("◀ pid.update"), "{text}");
    assert!(
        lines[0].starts_with("0.000000 "),
        "time is relative to the first line"
    );
}
