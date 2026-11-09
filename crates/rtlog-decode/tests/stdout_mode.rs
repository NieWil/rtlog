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

/// Feed `frames` to a fresh transport one write at a time, as a drain would,
/// and return the text it printed.
fn print_frames(frames: &[Vec<u8>]) -> String {
    let mut t = TextTransport::new(rtlog_decode::stdout::own_contract(), Vec::new(), false);
    for f in frames {
        rtlog::Transport::write(&mut t, f).expect("a text transport never reports full");
    }
    rtlog::Transport::flush(&mut t);
    String::from_utf8(t.get_ref().clone()).unwrap()
}

fn header() -> Vec<u8> {
    let hash = rtlog_decode::stdout::own_contract().hash();
    rtlog::wire::Header::new(hash, 1_000_000_000)
        .encode()
        .to_vec()
}

fn ctx_register(ctx: u8, name: &str) -> Vec<u8> {
    let mut f = rtlog::wire::CTRL_CTX_REGISTER.to_le_bytes().to_vec();
    f.push(ctx);
    f.push(name.len() as u8);
    f.extend_from_slice(name.as_bytes());
    f
}

#[test]
fn text_transport_reports_a_bad_header_and_takes_the_next_write_as_header() {
    let text = print_frames(&[
        b"not a header at all".to_vec(),
        header(),
        ctx_register(0, "x"),
    ]);
    assert_eq!(
        text, "rtlog: not an rtlog stream (bad or missing header)\n",
        "the context registration after the good header prints nothing"
    );
}

#[test]
fn text_transport_reports_an_unsupported_wire_version() {
    let mut h = header();
    h[4] = 9; // wire version
    assert_eq!(print_frames(&[h]), "rtlog: unsupported wire version 9\n");
}

#[test]
fn text_transport_accepts_a_contract_hash_mismatch() {
    // The live mode always decodes with the binary's own contract, so it forces.
    let mut h = header();
    h[6] ^= 0xff; // contract hash, first byte
    let mut dropped = rtlog::wire::CTRL_DROPPED.to_le_bytes().to_vec();
    dropped.extend_from_slice(&[0, 1, 0, 0, 0]); // ctx 0, count 1
    let text = print_frames(&[h, dropped]);
    assert!(text.ends_with("[ctx0] DROP  1 frames dropped\n"), "{text}");
}

#[test]
fn text_transport_prints_drop_reports_with_the_context_name() {
    let mut dropped = rtlog::wire::CTRL_DROPPED.to_le_bytes().to_vec();
    dropped.push(3); // ctx varint
    dropped.extend_from_slice(&42u32.to_le_bytes());
    let text = print_frames(&[header(), ctx_register(3, "sensor"), dropped]);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert!(
        lines[0].contains("[sensor] DROP  42 frames dropped"),
        "{text}"
    );
}

#[test]
fn text_transport_prints_crash_and_snapshot_frames() {
    let crash = |kind: u8, msg: &str| {
        let mut f = rtlog::wire::CTRL_CRASH.to_le_bytes().to_vec();
        f.extend_from_slice(&[0, 0, kind]); // ts delta, ctx, kind
        f.extend_from_slice(&0i32.to_le_bytes()); // signal
        f.extend_from_slice(&0i32.to_le_bytes()); // code
        f.extend_from_slice(&0u64.to_le_bytes()); // fault address
        f.extend_from_slice(&0u64.to_le_bytes()); // pc
        f.push(msg.len() as u8);
        f.extend_from_slice(msg.as_bytes());
        f
    };
    let text = print_frames(&[
        header(),
        crash(rtlog::wire::CRASH_PANIC, "boom"),
        crash(3, "watchdog"),
    ]);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].ends_with("[ctx0] CRASH panic: boom"), "{text}");
    assert!(
        lines[1].ends_with("[ctx0] SNAP  snapshot: watchdog"),
        "{text}"
    );
}

#[test]
fn text_transport_reports_a_bad_frame_and_keeps_going() {
    let mut close = rtlog::wire::CTRL_CTX_CLOSE.to_le_bytes().to_vec();
    close.push(0);
    let text = print_frames(&[
        header(),
        0x1234_5678u32.to_le_bytes().to_vec(), // no such site
        vec![1, 2],                            // truncated site ID
        close,                                 // a known frame still decodes, silently
    ]);
    assert_eq!(
        text,
        "rtlog: offset 0: unknown site 0x12345678\n\
         rtlog: offset 0: bad frame: truncated site ID\n",
    );
}
