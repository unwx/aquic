use crate::conditional;

conditional! {
    not(multithread),

    pub(crate) mod event;
}
