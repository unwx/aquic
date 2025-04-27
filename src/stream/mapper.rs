use bytes::Bytes;

pub trait StreamReaderFactory<M, SR>
where
    SR: StreamReader<M>,
{
    fn new_stream_reader(&self) -> SR;
}

pub trait StreamWriterFactory<M, SW>
where
    SW: StreamWriter<M>,
{
    fn new_stream_writer(&self) -> SW;
}


pub trait StreamReader<M> {
    fn buffer(&mut self) -> &mut [u8];

    fn next_message(&mut self) -> Option<M>;

    // TODO(docs)
    //  u64: error code to close a stream with.
    fn notify_read(&mut self, length: usize, fin: bool) -> Result<(), u64>;
}

pub trait StreamWriter<M> {
    fn write(&mut self, message: M) -> Bytes;
}
