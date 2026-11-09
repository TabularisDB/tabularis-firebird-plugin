//! Arc4 stream cipher implementation for the firebird wire encryption (Wire Protocol 13)

#[derive(Clone)]
pub struct Arc4 {
    i: u8,
    j: u8,
    state: [u8; 256],
}

impl Arc4 {
    pub fn new(key: &[u8]) -> Arc4 {
        assert!(!key.is_empty() && key.len() <= 256);
        let mut rc4 = Arc4 {
            i: 0,
            j: 0,
            state: [0; 256],
        };
        for (i, x) in rc4.state.iter_mut().enumerate() {
            *x = i as u8;
        }
        let mut j: u8 = 0;
        for i in 0..256 {
            j = j
                .wrapping_add(rc4.state[i])
                .wrapping_add(key[i % key.len()]);
            rc4.state.swap(i, j as usize);
        }
        rc4
    }

    pub(crate) fn next(&mut self) -> u8 {
        self.i = self.i.wrapping_add(1);
        self.j = self.j.wrapping_add(self.state[self.i as usize]);
        self.state.swap(self.i as usize, self.j as usize);

        self.state[(self.state[self.i as usize].wrapping_add(self.state[self.j as usize])) as usize]
    }

    fn process(&mut self, input: &[u8], output: &mut [u8]) {
        assert!(input.len() == output.len());
        for (x, y) in input.iter().zip(output.iter_mut()) {
            *y = *x ^ self.next();
        }
    }
}

#[test]
fn arc4_test() {
    let mut a1 = Arc4::new(b"a key");

    let enc = &mut [0; 10];

    a1.process(b"plain text", enc);
    assert_eq!(enc, b"\x4b\x4b\xdc\x65\x02\xb3\x08\x17\x48\x82");

    let mut a2 = Arc4::new(b"a key");

    let plain = &mut [0; 10];

    a2.process(enc, plain);
    assert_eq!(plain, b"plain text");
}
