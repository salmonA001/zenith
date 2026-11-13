//! One big vertex buffer, handed out in pieces. A chunk's meshes come and go constantly, and making
//! a GPU buffer for each one would cost more than everything else in the frame.
use std::collections::BTreeMap;

/// A range of vertices in the arena.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub len: u32,
}

pub struct Arena {
    /// Free runs, by where they start.
    free: BTreeMap<u32, u32>,
    pub used: u32,
    pub capacity: u32,
}

impl Arena {
    pub fn new(capacity: u32) -> Self {
        let mut free = BTreeMap::new();
        free.insert(0, capacity);
        Self { free, used: 0, capacity }
    }

    /// Finds room for `len` vertices. The lowest fitting run keeps the buffer's top end free.
    pub fn alloc(&mut self, len: u32) -> Option<Span> {
        if len == 0 {
            return Some(Span::default());
        }
        let (&start, &size) = self.free.iter().find(|&(_, &size)| size >= len)?;
        self.free.remove(&start);
        if size > len {
            self.free.insert(start + len, size - len);
        }
        self.used += len;
        Some(Span { start, len })
    }

    /// Gives a span back, joining it to whatever free space touches it.
    pub fn release(&mut self, span: Span) {
        if span.len == 0 {
            return;
        }
        self.used -= span.len;
        let (mut start, mut len) = (span.start, span.len);
        if let Some((&before, &size)) = self.free.range(..start).next_back()
            && before + size == start
        {
            self.free.remove(&before);
            start = before;
            len += size;
        }
        if let Some(&size) = self.free.get(&(start + len)) {
            self.free.remove(&(start + len));
            len += size;
        }
        self.free.insert(start, len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenith_core::Rng;

    #[test]
    fn pieces_never_overlap_and_fit_in_the_buffer() {
        let mut rng = Rng::new(1);
        let mut arena = Arena::new(100_000);
        let mut live: Vec<Span> = Vec::new();
        for _ in 0..5000 {
            if rng.below(3) > 0 || live.is_empty() {
                if let Some(s) = arena.alloc(rng.below(2000) + 1) {
                    assert!(s.start + s.len <= arena.capacity);
                    assert!(live.iter().all(|o| s.start + s.len <= o.start || o.start + o.len <= s.start), "overlap");
                    live.push(s);
                }
            } else {
                let i = rng.below(live.len() as u32) as usize;
                arena.release(live.swap_remove(i));
            }
        }
        assert_eq!(arena.used, live.iter().map(|s| s.len).sum::<u32>());
    }

    #[test]
    fn freeing_everything_leaves_one_whole_run() {
        let mut arena = Arena::new(1000);
        let spans: Vec<Span> = (0..10).map(|_| arena.alloc(100).expect("fits")).collect();
        assert!(arena.alloc(1).is_none(), "full");
        for s in spans {
            arena.release(s);
        }
        assert_eq!(arena.used, 0);
        assert_eq!(arena.alloc(1000), Some(Span { start: 0, len: 1000 }));
    }

    #[test]
    fn an_empty_mesh_takes_no_room() {
        let mut arena = Arena::new(10);
        assert_eq!(arena.alloc(0), Some(Span::default()));
        arena.release(Span::default());
        assert_eq!(arena.used, 0);
    }
}
