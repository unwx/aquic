use crate::conditional;

conditional! {
    not(any(feature = "async-send")),

    pub(crate) mod event;
}
