extern crate alloc;

use self::alloc::raw_vec::RawVec;
use std::io::{Read, Write, Seek, SeekFrom, Error, ErrorKind, Result};
use std::slice;
use std::sync::{Arc, RwLock};
use std::sync::atomic::{AtomicUsize, Ordering};
use next_reader::NextReader;

/// A byte buffer that can only be appended to.
/// Once added, elements cannot be removed or replaced.
struct AppendVec {
    data: RawVec<u8>,
    len: AtomicUsize,
}

impl AppendVec {
    fn with_capacity(cap: usize) -> AppendVec {
        AppendVec {
            data: RawVec::with_capacity(cap),
            len: AtomicUsize::new(0),
        }
    }

    fn len(&self) -> usize {
        self.len.load(Ordering::SeqCst)
    }

    unsafe fn append(&self, buf: &[u8]) -> Result<Option<AppendVec>> {
        if self.data.cap() - self.len() >= buf.len() {
            try!(self.write(buf));
            return Ok(None);
        }
        let new_vec = AppendVec::with_capacity(self.len() * 2 + buf.len());
        let data = slice::from_raw_parts(self.data.ptr(), self.len());
        try!(new_vec.write(data));
        try!(new_vec.write(buf));
        Ok(Some(new_vec))
    }

    // Unsafe because this type is sync but can only safely have
    // one writer.
    unsafe fn write(&self, buf: &[u8]) -> Result<usize> {
        let len = self.len();
        let write_start = self.data.ptr().offset(len as isize);
        let remaining = self.data.cap() - len;
        let mut writer = slice::from_raw_parts_mut(write_start, remaining);
        let amount_written = try!(writer.write(buf));
        self.len.fetch_add(amount_written, Ordering::SeqCst);
        Ok(amount_written)
    }

    fn read(&self, buf: &mut [u8], from: usize) -> Result<usize> {
        let mut reader = unsafe {
            let len = self.len();
            // Can't read past the immutable portion.
            assert!(from <= len); // TODO(djherbis): return io::Error

            // Safe because we can always safely read concurrently
            // from the immutable portion, and we've checked that
            // we're not reading into the mutable portion.
            let read_start = self.data.ptr().offset(from as isize);
            slice::from_raw_parts(read_start, len - from)
        };

        let amount_read = try!(reader.read(buf));
        Ok(amount_read)
    }
}

pub struct Buffer {
    data: Arc<RwLock<Arc<AppendVec>>>,
    len: usize,
}

impl Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        // Safe because there is only ever one writer
        let mut data = self.data.write().unwrap();
        if let Some(new_data) = unsafe { try!(data.append(buf)) } {
            *data = Arc::new(new_data)
        }
        self.len = data.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

impl Buffer {
    pub fn new(cap: usize) -> Buffer {
        Buffer {
            data: Arc::new(RwLock::new(Arc::new(AppendVec::with_capacity(cap)))),
            len: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

impl NextReader for Buffer {
    type Reader = Reader;

    fn reader(&self) -> Result<Reader> {
        return Ok(Reader {
            read_start: 0,
            data: self.data.clone(),
        });
    }
}

pub struct Reader {
    data: Arc<RwLock<Arc<AppendVec>>>,
    read_start: usize,
}

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let amount_read = try!(self.data.read().unwrap().clone().read(buf, self.read_start));
        self.read_start += amount_read;
        Ok(amount_read)
    }
}

impl Seek for Reader {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        let current = self.read_start as i64;
        let end = self.data.read().unwrap().clone().len() as i64;

        let new_pos = match pos {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => current + n,
            SeekFrom::End(n) => end + n,
        };

        if new_pos < 0 || new_pos > end {
            return Err(Error::new(ErrorKind::InvalidInput,
                                  format!("invalid seek pos {}", new_pos)));
        }

        self.read_start = new_pos as usize;
        Ok(new_pos as u64)
    }
}

#[test]
fn it_buffers() {
    let mut writer = Buffer::new(1);
    let mut reader = writer.reader().unwrap();
    writer.write(b"hello").unwrap();

    let mut bytes = [0; 11];
    assert_eq!(reader.read(&mut bytes).unwrap(), 5);
    assert_eq!(&bytes[..5], b"hello");


    writer.write(b" world").unwrap();
    let bytes = ::std::thread::spawn(move || {
            assert_eq!(reader.read(&mut bytes[5..]).unwrap(), 6);
            bytes
        })
        .join()
        .unwrap();
    assert_eq!(&bytes, b"hello world");
}
