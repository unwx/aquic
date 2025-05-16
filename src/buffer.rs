use bytes::Bytes;
use quiche::{BufFactory, BufSplit};

#[derive(Debug, Clone)]
pub struct BufView {
    bytes: Bytes,
}

impl From<Bytes> for BufView {
    fn from(bytes: Bytes) -> Self {
        Self { bytes }
    }
}

impl Into<Bytes> for BufView {
    fn into(self) -> Bytes {
        self.bytes
    }
}

impl AsRef<[u8]> for BufView {
    fn as_ref(&self) -> &[u8] {
        self.bytes.as_ref()
    }
}

impl BufSplit for BufView {
    fn split_at(&mut self, at: usize) -> Self {
        BufView::from(self.bytes.split_off(at))
    }
}


#[derive(Debug, Copy, Clone, Default)]
pub struct BufViewFactory;

impl BufFactory for BufViewFactory {
    type Buf = BufView;

    fn buf_from_slice(buf: &[u8]) -> Self::Buf {
        BufView::from(Bytes::copy_from_slice(buf))
    }
}
