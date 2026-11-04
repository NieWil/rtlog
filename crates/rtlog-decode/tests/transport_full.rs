mod common;

#[test]
fn transport_full_keeps_frames_for_the_next_poll() {
    struct Tiny(Vec<u8>, usize);
    impl rtlog::Transport for Tiny {
        fn write(&mut self, f: &[u8]) -> Result<(), rtlog::Full> {
            if self.0.len() + f.len() > self.1 {
                return Err(rtlog::Full);
            }
            self.0.extend_from_slice(f);
            Ok(())
        }
    }
    std::thread::spawn(|| {
        for i in 0..20u32 {
            rtlog::info!("kept {}", i);
        }
    })
    .join()
    .unwrap();
    let mut d = rtlog::Drain::new(Tiny(Vec::new(), 64));
    assert_eq!(d.poll(), Err(rtlog::Full));
    let first = d.transport().0.len();
    d.transport_mut().1 = usize::MAX;
    d.poll().unwrap();
    assert!(d.transport().0.len() > first);
}
