extern crate time;

use std::marker::PhantomData;
use std::mem;
use std::ptr;

use traits::{Trace, TraceId, TraceSink};

#[derive(Clone, Debug)]
pub struct RingBuffer<T> {
    // The data itself.
    data: Vec<u8>,

    // Where valid data begins.
    begin: usize,

    // The number of bytes in the ring buffer that are valid.
    length: usize,

    phantom: PhantomData<T>,
}

impl<T> Default for RingBuffer<T> {
    fn default() -> RingBuffer<T> {
        Self::new(4096)
    }
}

impl<T> RingBuffer<T> {
    pub fn new(capacity: usize) -> RingBuffer<T> {
        assert!(capacity > TraceEntry::<T>::size());
        RingBuffer {
            data: vec![0; capacity],
            begin: 0,
            length: 0,
            phantom: PhantomData,
        }
    }

    pub fn iter(&self) -> RingBufferIter<T> {
        RingBufferIter(if self.length == 0 {
            RingBufferIterState::Empty
        } else {
            RingBufferIterState::NonEmpty {
                buffer: self,
                idx: self.begin,
            }
        })
    }

    #[inline(always)]
    fn end(&self) -> usize {
        (self.begin + self.length) % self.data.len()
    }

    fn write(&mut self, data: &[u8]) {
        let end = self.end();
        let new_data_len = data.len();
        let capacity = self.data.len();

        if capacity - self.length < TraceEntry::<T>::size() {
            self.begin = (self.begin + TraceEntry::<T>::size()) % capacity;
            self.length -= TraceEntry::<T>::size();
        }

        if end + new_data_len > capacity {
            let middle = capacity - end;
            self.data[end..capacity].copy_from_slice(&data[..middle]);
            self.data[0..new_data_len - middle].copy_from_slice(&data[middle..]);
        } else {
            self.data[end..end + new_data_len].copy_from_slice(data);
        }

        self.length += TraceEntry::<T>::size();
        debug_assert!(self.length <= capacity);
    }
}

impl<T> TraceSink<T> for RingBuffer<T>
    where T: Trace
{
    fn trace_event(&mut self, trace: T, _why: Option<T::Id>) -> T::Id {
        let entry: TraceEntry<T> = TraceEntry {
            timestamp: NsSinceEpoch::now(),
            tag: trace.tag(),
            kind: TraceKind::Event,
            phantom: PhantomData,
        };
        let entry: [u8; 13] = unsafe { mem::transmute(entry) };
        self.write(&entry);
        T::Id::new_id()
    }

    fn trace_start(&mut self, trace: T, _why: Option<T::Id>) -> T::Id {
        let entry: TraceEntry<T> = TraceEntry {
            timestamp: NsSinceEpoch::now(),
            tag: trace.tag(),
            kind: TraceKind::Start,
            phantom: PhantomData,
        };
        let entry: [u8; 13] = unsafe { mem::transmute(entry) };
        self.write(&entry);
        T::Id::new_id()
    }

    fn trace_stop(&mut self, trace: T) {
        let entry: TraceEntry<T> = TraceEntry {
            timestamp: NsSinceEpoch::now(),
            tag: trace.tag(),
            kind: TraceKind::Stop,
            phantom: PhantomData,
        };
        let entry: [u8; 13] = unsafe { mem::transmute(entry) };
        self.write(&entry);
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NsSinceEpoch(pub u64);

impl NsSinceEpoch {
    #[inline(always)]
    pub fn now() -> NsSinceEpoch {
        let timespec = time::get_time();
        let sec = timespec.sec as u64;
        let nsec = timespec.nsec as u64;
        NsSinceEpoch(sec * 1_000_000_000 + nsec)
    }
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TraceKind {
    Event = 0x0,
    Start = 0x1,
    Stop = 0x2,
}

#[repr(packed)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TraceEntry<T> {
    timestamp: NsSinceEpoch,
    tag: u32,
    kind: TraceKind,
    phantom: PhantomData<T>,
}

impl<T> TraceEntry<T>
    where T: Trace
{
    pub fn label(&self) -> &'static str {
        T::label(self.tag)
    }
}

impl<T> TraceEntry<T> {
    pub fn tag(&self) -> u32 {
        self.tag
    }

    pub fn kind(&self) -> TraceKind {
        self.kind
    }

    fn size() -> usize {
        mem::size_of::<Self>()
    }
}

#[derive(Clone, Debug)]
enum RingBufferIterState<'a, T>
    where T: 'a
{
    Empty,
    NonEmpty {
        buffer: &'a RingBuffer<T>,
        idx: usize,
    },
}

#[derive(Clone, Debug)]
pub struct RingBufferIter<'a, T>(RingBufferIterState<'a, T>) where T: 'a;

impl<'a, T> Iterator for RingBufferIter<'a, T> {
    type Item = TraceEntry<T>;

    fn next(&mut self) -> Option<Self::Item> {
        let (next_state, result) = match self.0 {
            RingBufferIterState::Empty => return None,
            RingBufferIterState::NonEmpty { ref buffer, idx } => {
                let result = unsafe {
                    if idx + TraceEntry::<T>::size() > buffer.data.len() {
                        let mut temp = [0; 13];
                        let middle = buffer.data.len() - idx;
                        temp[..middle].copy_from_slice(&buffer.data[idx..]);
                        temp[middle..]
                            .copy_from_slice(&buffer.data[..TraceEntry::<T>::size() - middle]);
                        Some(mem::transmute(temp))
                    } else {
                        let entry_ptr = buffer.data[idx..].as_ptr() as *const TraceEntry<T>;
                        Some(ptr::read(entry_ptr))
                    }
                };

                let next_idx = (idx + TraceEntry::<T>::size()) % buffer.data.len();
                let next_state = if next_idx == buffer.end() {
                    RingBufferIterState::Empty
                } else {
                    RingBufferIterState::NonEmpty {
                        buffer: buffer,
                        idx: next_idx,
                    }
                };

                (next_state, result)
            }
        };

        mem::replace(&mut self.0, next_state);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use simple_trace::{SimpleTrace, SimpleTraceBuffer};
    use traits::{Trace, TraceSink};

    type SimpleTraceEntry = TraceEntry<SimpleTrace>;

    #[test]
    fn trace_entry_has_right_size() {
        assert_eq!(SimpleTraceEntry::size(), 13);
    }

    #[test]
    fn no_roll_over() {
        let mut buffer = SimpleTraceBuffer::new(100 * SimpleTraceEntry::size());
        buffer.trace_event(SimpleTrace::FooEvent, None);
        buffer.trace_start(SimpleTrace::OperationThing, None);
        buffer.trace_start(SimpleTrace::OperationAnother, None);
        buffer.trace_event(SimpleTrace::FooEvent, None);
        buffer.trace_stop(SimpleTrace::OperationThing);
        buffer.trace_stop(SimpleTrace::OperationAnother);

        let mut iter = buffer.iter();

        let entry = iter.next().unwrap();
        assert_eq!(entry.tag(), SimpleTrace::FooEvent.tag());
        assert_eq!(entry.kind(), TraceKind::Event);
        assert_eq!(entry.label(), "Foo");

        let entry = iter.next().unwrap();
        assert_eq!(entry.tag(), SimpleTrace::OperationThing.tag());
        assert_eq!(entry.kind(), TraceKind::Start);
        assert_eq!(entry.label(), "Thing");

        let entry = iter.next().unwrap();
        assert_eq!(entry.tag(), SimpleTrace::OperationAnother.tag());
        assert_eq!(entry.kind(), TraceKind::Start);
        assert_eq!(entry.label(), "Another");

        let entry = iter.next().unwrap();
        assert_eq!(entry.tag(), SimpleTrace::FooEvent.tag());
        assert_eq!(entry.kind(), TraceKind::Event);
        assert_eq!(entry.label(), "Foo");

        let entry = iter.next().unwrap();
        assert_eq!(entry.tag(), SimpleTrace::OperationThing.tag());
        assert_eq!(entry.kind(), TraceKind::Stop);
        assert_eq!(entry.label(), "Thing");

        let entry = iter.next().unwrap();
        assert_eq!(entry.tag(), SimpleTrace::OperationAnother.tag());
        assert_eq!(entry.kind(), TraceKind::Stop);
        assert_eq!(entry.label(), "Another");

        assert_eq!(iter.next(), None);
    }

    #[test]
    fn with_roll_over() {
        let mut buffer = SimpleTraceBuffer::new(5 * SimpleTraceEntry::size());
        buffer.trace_event(SimpleTrace::FooEvent, None);
        buffer.trace_start(SimpleTrace::OperationThing, None);
        buffer.trace_start(SimpleTrace::OperationAnother, None);
        buffer.trace_event(SimpleTrace::FooEvent, None);
        buffer.trace_stop(SimpleTrace::OperationThing);
        buffer.trace_stop(SimpleTrace::OperationAnother);

        println!("buffer = {:#?}", buffer);

        let mut iter = buffer.iter();

        let entry = iter.next().unwrap();
        println!("entry = {:#?}", entry);
        assert_eq!(entry.tag(), SimpleTrace::OperationThing.tag());
        assert_eq!(entry.kind(), TraceKind::Start);
        assert_eq!(entry.label(), "Thing");

        let entry = iter.next().unwrap();
        println!("entry = {:#?}", entry);
        assert_eq!(entry.tag(), SimpleTrace::OperationAnother.tag());
        assert_eq!(entry.kind(), TraceKind::Start);
        assert_eq!(entry.label(), "Another");

        let entry = iter.next().unwrap();
        println!("entry = {:#?}", entry);
        assert_eq!(entry.tag(), SimpleTrace::FooEvent.tag());
        assert_eq!(entry.kind(), TraceKind::Event);
        assert_eq!(entry.label(), "Foo");

        let entry = iter.next().unwrap();
        println!("entry = {:#?}", entry);
        assert_eq!(entry.tag(), SimpleTrace::OperationThing.tag());
        assert_eq!(entry.kind(), TraceKind::Stop);
        assert_eq!(entry.label(), "Thing");

        let entry = iter.next().unwrap();
        println!("entry = {:#?}", entry);
        assert_eq!(entry.tag(), SimpleTrace::OperationAnother.tag());
        assert_eq!(entry.kind(), TraceKind::Stop);
        assert_eq!(entry.label(), "Another");

        assert_eq!(iter.next(), None);
    }

    #[test]
    fn with_roll_over_and_does_not_divide_evenly() {
        let mut buffer = SimpleTraceBuffer::new(3 * SimpleTraceEntry::size() + 1);
        buffer.trace_event(SimpleTrace::FooEvent, None);
        buffer.trace_start(SimpleTrace::OperationThing, None);
        buffer.trace_start(SimpleTrace::OperationAnother, None);
        buffer.trace_event(SimpleTrace::FooEvent, None);
        buffer.trace_stop(SimpleTrace::OperationThing);
        buffer.trace_stop(SimpleTrace::OperationAnother);

        println!("buffer = {:#?}", buffer);

        let mut iter = buffer.iter();

        let entry = iter.next().unwrap();
        println!("entry = {:#?}", entry);
        assert_eq!(entry.tag(), SimpleTrace::FooEvent.tag());
        assert_eq!(entry.kind(), TraceKind::Event);
        assert_eq!(entry.label(), "Foo");

        let entry = iter.next().unwrap();
        println!("entry = {:#?}", entry);
        assert_eq!(entry.tag(), SimpleTrace::OperationThing.tag());
        assert_eq!(entry.kind(), TraceKind::Stop);
        assert_eq!(entry.label(), "Thing");

        let entry = iter.next().unwrap();
        println!("entry = {:#?}", entry);
        assert_eq!(entry.tag(), SimpleTrace::OperationAnother.tag());
        assert_eq!(entry.kind(), TraceKind::Stop);
        assert_eq!(entry.label(), "Another");

        assert_eq!(iter.next(), None);
    }
}