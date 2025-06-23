use bytes::Bytes;

// TODO(docs):
//  All async methods must be cancel-safe.
//  All async methods can be implementable as synchronous without significant performance impact
//   if no I/O blocking is anticipated.

pub trait StreamReaderFactory<T, E, SR>
where
    SR: StreamReader<T, E>,
{
    fn new_stream_reader(&self) -> SR;
}

pub trait StreamWriterFactory<T, E, SW>
where
    SW: StreamWriter<T, E>,
{
    fn new_stream_writer(&self) -> SW;
}


pub trait StreamReader<T, E> {
    // TODO(docs):
    //  Never empty
    fn buffer(&mut self) -> &mut [u8];

    // TODO(docs):
    //  `length` can equal '0' if `finish` is 'true'.
    fn notify_read(
        &mut self,
        length: usize,
        finish: bool,
    ) -> impl Future<Output = Result<(), E>> + Send;

    fn has_message(&self) -> bool;

    fn next_message(&mut self) -> impl Future<Output = Result<T, E>> + Send;
}

pub trait StreamWriter<T, E> {
    // TODO(docs):
    //  `message` can be `None` if `finish` is 'true'.
    fn write(&mut self, message: Option<T>, finish: bool) -> impl Future<Output = Result<(), E>> + Send;

    fn has_buffer(&self) -> bool;

    fn next_buffer(&mut self) -> impl Future<Output = Result<Bytes, E>> + Send;
}
