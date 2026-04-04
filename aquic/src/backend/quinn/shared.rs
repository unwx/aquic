use crate::{net::Ecn, stream::StreamId};
use quinn_proto::{EcnCodepoint, VarInt};

impl From<Ecn> for Option<EcnCodepoint> {
    fn from(value: Ecn) -> Self {
        match value {
            Ecn::NEct => None,
            Ecn::Ect1 => Some(EcnCodepoint::Ect1),
            Ecn::Ect0 => Some(EcnCodepoint::Ect0),
            Ecn::Ce => Some(EcnCodepoint::Ce),
        }
    }
}

impl From<Option<EcnCodepoint>> for Ecn {
    fn from(value: Option<EcnCodepoint>) -> Self {
        match value {
            Some(ecn) => match ecn {
                EcnCodepoint::Ect0 => Ecn::Ect0,
                EcnCodepoint::Ect1 => Ecn::Ect1,
                EcnCodepoint::Ce => Ecn::Ce,
            },
            None => Ecn::NEct,
        }
    }
}

impl From<quinn_proto::StreamId> for StreamId {
    fn from(value: quinn_proto::StreamId) -> Self {
        StreamId::new(u64::from(value)).unwrap_or_else(|e| {
            panic!("unable to convert `quinn_proto::StreamId` ({value}) into aquic::stream::StreamId: {e}");
        })
    }
}

impl From<StreamId> for quinn_proto::VarInt {
    fn from(value: StreamId) -> Self {
        quinn_proto::VarInt::from_u64(value.raw()).unwrap_or_else(|e| {
            panic!("unable to convert `aquic::stream::StreamId` ({value}) into quinn_proto::VarInt: {e}");
        })
    }
}

impl From<StreamId> for quinn_proto::StreamId {
    fn from(value: StreamId) -> Self {
        quinn_proto::StreamId::from(VarInt::from(value))
    }
}
