mod dgram;
mod stream;

pub(crate) use dgram::*;
pub(crate) use stream::*;

pub(crate) enum AquicCommand<CId> {
    _Private(CId), // TODO(feat): different commands should be here.
}

pub(crate) enum AquicResponse {
    // TODO(feat): different responses should be here.
}
